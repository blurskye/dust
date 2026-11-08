//! Application effects and command registration.

use std::path::PathBuf;

use crate::editor::ArgumentSchema;

use crate::editor::ArgumentType;

use crate::editor::CommandError;

use crate::editor::CommandOutcome;

use crate::editor::CommandRegistry;

use crate::editor::CommandSpec;

use crate::editor::CommandValue;

use crate::editor::ContextRequirement;

use crate::editor::input::GrammarInput;
use crate::editor::{CommandAuthority, CommandWrapperAction, PendingCommandId};
use crate::ui::ConfirmationChoice;
use crate::ui::PromptKind;
use crate::windows::SpatialDirection;

#[derive(Clone, Copy, Debug)]
pub enum SyntaxFoldAction {
    Toggle,
    Open,
    Close,
    OpenAll,
    CloseAll,
}

#[derive(Clone, Debug)]
pub enum AppEffect {
    Authorized {
        authority: CommandAuthority,
        effect: Box<AppEffect>,
    },
    CommandResume {
        pending: PendingCommandId,
        owner: String,
        action: Box<CommandWrapperAction<AppEffect>>,
    },
    Input(crate::app::input::InputCommand),
    Quit {
        force: bool,
    },
    Save {
        quit_after: bool,
        path: Option<PathBuf>,
        force: bool,
    },
    OpenPath(PathBuf),
    /// `:e! path`: open it, and if it is the current file, reload it and drop unsaved edits.
    ForceOpenPath(PathBuf),
    OpenUserFile(PathBuf),
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
    GitBrowse {
        copy: bool,
    },
    FlashBegin,
    TodoSearch,
    /// An agent command; `dispatch/agent/commands.rs` names and runs them.
    Agent(crate::app::agent::commands::AgentCommand),
    NewTab,
    PullRequestList,
    PullRequestComments,
    PullRequestComment,
    /// Switch the change surface's diff pane between the unstaged and staged comparison.
    GitChangesLayerSwitch,
    GitHunksOpen,
    GitHunkNavigate(i8),
    GitReviewHunk(String),
    GitConflictNavigate(i8),
    MarkdownPreview,
    SaveAll {
        quit_after: bool,
    },
    MarkJump(String),
    GitHunkPreview,
    GitHunkStage,
    GitHunkReset,
    GitHunksOpenStaged,
    GitHunkUnstage,
    GitWorktreesOpen,
    GitWorktreeRemove,
    GitWorktreeOpen,
    GitConflictsOpen,
    GitConflictChoose {
        side: String,
    },
    DebuggerBreakpointStates,
    DebuggerBreakpointDisable,
    DebuggerTerminate,
    DebuggerDetach,
    GitConflictStage,
    GitConflictContinue,
    GitConflictAbort,
    GitConflictReplace {
        side: String,
        files: String,
    },
    Tutor,
    TutorNext,
    TutorBack,
    TutorQuit,
    /// `ctrl-o` (true) or `ctrl-i` through the jump list.
    WalkJumps(bool),
    HelpContextual,
    HelpCommandReference,
    HelpForForm,
    GitWorktreeCreate {
        branch: String,
        path: Option<std::path::PathBuf>,
    },
    GitFileStage,
    GitFileUnstage,
    GitLinesStage {
        start: i64,
        end: i64,
    },
    OpenPrompt {
        kind: PromptKind,
        initial: String,
    },
    PromptInsert(String),
    PromptBackspace,
    PromptDeleteWord,
    PromptDeleteToStart,
    PromptMove(PromptMove),
    PromptHistory(i8),
    PromptComplete(i8),
    PromptConfirmationSelect(ConfirmationChoice),
    PromptConfirmationToggle,
    PromptSubmit,
    PromptCancel,
    NotificationHistory,
    NotificationDismiss,
    SearchRepeat {
        reverse: bool,
        count: u64,
    },
    SearchSelectNext(u64),
    SubstituteAll,
    SearchWord {
        forward: bool,
        count: u64,
    },
    Scroll {
        kind: String,
        count: u64,
        at_mouse: bool,
    },
    PlaceMouseCursor,
    ExtendMouseSelection,
    Layout(LayoutAction),
    SurfaceFocus(SpatialDirection),
    /// Moves the dashboard cursor by this many items.
    DashboardSelect(i8),
    DashboardOpen,
    WindowList,
    WindowClone {
        target: String,
        axis: crate::windows::TileAxis,
    },
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
    WindowResizeDirectional(SpatialDirection, u64),
    WindowMove {
        target: String,
        x: u16,
        y: u16,
    },
    WindowSwap {
        first: String,
        second: String,
    },
    RepeatLast(Option<std::num::NonZeroU64>),
    MacroRecordStart(char),
    MacroRecordStop,
    MacroReplay {
        register: char,
        count: u64,
    },
    Grammar(GrammarInput),
    CancelPendingMapping,
    ReloadConfiguration,
    SelectTheme,
    /// `:colorscheme name`: apply it without the list.
    SetTheme(String),
    ExplainConfig(String),
    InspectKeymap {
        id: Option<String>,
        context: Option<String>,
        key: Option<String>,
        command: Option<String>,
    },
    ShowKeymapConflicts,
    RecoveryInspect,
    SessionSave,
    SessionLoad,
    HistoryClear,
    LargeFileSet(String),
    LargeFileStatus,
    SyntaxStatus,
    SyntaxFold(SyntaxFoldAction),
    LspRenamePrompt,
    GitConflictAccept {
        side: String,
    },
    Substitute {
        range: String,
        pattern: String,
        replacement: String,
        flags: String,
    },
    GlobalDelete {
        range: String,
        pattern: String,
        invert: bool,
    },
    LspCompletionLeave,
    SyntaxIndentLine,
    TaskRun(String),
    TaskStop(String),
    TaskOutput(String),
    PluginValidate,
    PluginList,
    PluginDisable(String),
    PluginCall {
        plugin: String,
        handler: String,
        call: Box<crate::plugins::component::component_api::dust::plugin::host::CommandCall>,
        pending: Option<PendingCommandId>,
        previous: Option<Box<CommandOutcome<AppEffect>>>,
    },
    PluginExecute {
        plugin: String,
        command: String,
        arguments: Vec<String>,
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
    /// Without a rectangle the terminal floats centred, at most of the screen.
    TerminalFloat(Option<crate::windows::WindowRect>),
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
    TerminalQuit,
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
    WorkspaceUntrust,
    WorkspaceStatus,
    WorkspaceRootAdd(PathBuf),
    WorkspaceRootActivate(u64),
    WorkspaceRootRemove(u64),
    WorkspaceRootStatus,
    ExplorerOpen,
    ExplorerClose,
    ExplorerPanelToggle,
    ExplorerMove(i8),
    ExplorerPage(isize),
    ExplorerParent,
    ExplorerChild,
    ExplorerToggle,
    ExplorerRefresh,
    ExplorerFollow,
    ExplorerToggleHidden,
    ExplorerToggleIgnored,
    ExplorerSearch,
    ExplorerSearchInsert(String),
    ExplorerSearchDelete,
    ExplorerSearchStop {
        keep: bool,
    },
    ExplorerFileOpen {
        split: bool,
    },
    ExplorerCreate(String),
    ExplorerRename(String),
    ExplorerRenamePrompt,
    ExplorerDeleteRequest,
    ExplorerClipboard {
        cut: bool,
    },
    ExplorerPaste,
    ExplorerYankPath,
    ExplorerMouseSelect,
    WorkspaceSearchPrompt,
    WorkspaceSearch(String),
    WorkspaceSearchCancel,
    WorkspaceSearchResume,
    WorkspaceReplace {
        query: String,
        replacement: String,
    },
    OpenPicker(PickerKind),
    PickerInsert(String),
    PickerBackspace,
    PickerMove(i8),
    /// Page the open picker's diff pane.
    PickerPreviewPage(i8),
    /// Re-present the open picker's rows on the shared Results surface.
    PickerResults,
    /// Remove the selected row's thing: a recovery journal or a worktree.
    PickerRemove,
    PickerAccept,
    PickerCancel,
    PickerNormal,
    PickerInsertMode,
    WorkspaceEditToggle,
    WorkspaceEditAccept,
    WorkspaceEditReject,
    WorkspaceResourceApply,
    Settings(SettingsAction),
    Setup(SetupAction),
    OpenToolMenu(String),
    CloseToolMenu,
    MoveToolMenu(i8),
    ToolSearch,
    ToolSearchInsert(String),
    ToolSearchDelete,
    ToolSearchStop {
        keep: bool,
    },
    CycleToolCategory(i8),
    RefreshTools,
    ActivateTool,
    InstallSelectedTool,
    UninstallSelectedTool,
    StopSelectedLsp,
    RestartSelectedLsp,
    ToggleToolRuntime,
    LspStart,
    LspStop,
    LspRestart,
    LspStatus,
    LspLogs,
    LspSessions,
    LspSessionSelect(String),
    LspAttach,
    LspDetach,
    LspInlayHintsToggle,
    ViewToggle(crate::app::configuration::ViewToggle),
    Zen,
    LspRequest(crate::language::lsp::LspFeature),
    LspDocumentationScroll(isize),
    LspDocumentationClose,
    LspCompletionDocumentationOpen,
    LspCompletionMove(i8),
    LspCompletionAccept,
    LspCompletionInlineAccept,
    LspIncoming(i64),
    LspCompletionCancel,
    SnippetNavigate(i8),
    SnippetLeave,
    SnippetCancel,
    LspFoldToggle,
    LspSelectionExpand,
    DiagnosticNavigate {
        direction: i8,
        filter: DiagnosticFilter,
        count: u64,
    },
    DapStart(Option<PathBuf>),
    DapCursorTest,
    DapStop,
    DapContinue,
    DapPause,
    DapNext,
    DapStepIn,
    DapStepOut,
    DapStatus,
    DapConsole,
    DapSessions,
    DapInspectorNext,
    DapThreads,
    DapStack(Option<i64>),
    DapScopes(Option<i64>),
    DapVariables(Option<i64>),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticFilter {
    Error,
    Warning,
    ErrorOrWarning,
}

impl DiagnosticFilter {
    pub const fn severities(self) -> &'static [crate::workspace::DiagnosticSeverity] {
        use crate::workspace::DiagnosticSeverity::Error;
        use crate::workspace::DiagnosticSeverity::Warning;
        match self {
            Self::Error => &[Error],
            Self::Warning => &[Warning],
            Self::ErrorOrWarning => &[Error, Warning],
        }
    }
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

use crate::app::windows::LayoutAction;

/// What a key on the Settings page does.
#[derive(Clone, Debug)]
pub enum SettingsAction {
    Open,
    /// The page of key bindings.
    Shortcuts,
    Close,
    /// Move the selection by this many settings.
    Move(i64),
    /// Jump to the next or previous section.
    Section(i64),
    /// Enter: change the setting, or confirm the edit or choice.
    Activate,
    /// Space: flip an on/off setting; otherwise as Enter.
    Toggle,
    /// Remove the user's value so the default applies.
    Reset,
    Search,
    /// Show only the settings the user changed, or everything again.
    Modified,
    /// Open config.toml itself.
    OpenFile,
    /// Escape: leave the edit, choice or search, or close the page.
    Cancel,
    Insert(String),
    Backspace,
    Cursor(PromptMove),
    Clear,
}

/// What a key on the first-start setup does.
#[derive(Clone, Copy, Debug)]
pub enum SetupAction {
    Open,
    /// Highlight another choice on the current page.
    Move(i64),
    /// Take the highlighted choice and go to the next page, or save on the last one.
    Next,
    Back,
    /// Close without changing anything.
    Skip,
}

#[derive(Clone, Copy, Debug)]
pub enum PromptMove {
    Left,
    Right,
    /// To the start of the word or run of punctuation before the cursor, as Vim's `b`.
    WordLeft,
    /// To the start of the next word or run of punctuation, as Vim's `w`.
    WordRight,
    Home,
    End,
}

impl PromptMove {
    /// Where a cursor at byte `cursor` of a single text field moves to.
    #[must_use]
    pub fn apply(self, text: &str, cursor: usize) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        let class = |grapheme: &str| {
            let character = grapheme.chars().next().unwrap_or(' ');
            if character.is_whitespace() {
                0
            } else if character.is_alphanumeric() || character == '_' {
                1
            } else {
                2
            }
        };
        match self {
            Self::Home => 0,
            Self::End => text.len(),
            Self::Left => text[..cursor]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(index, _)| index),
            Self::Right => text[cursor..]
                .grapheme_indices(true)
                .nth(1)
                .map_or(text.len(), |(index, _)| cursor + index),
            Self::WordRight => {
                let mut graphemes = text[cursor..].grapheme_indices(true).peekable();
                let start = graphemes.peek().map(|(_, grapheme)| class(grapheme));
                graphemes
                    .skip_while(|(_, grapheme)| Some(class(grapheme)) == start)
                    .find(|(_, grapheme)| class(grapheme) != 0)
                    .map_or(text.len(), |(index, _)| cursor + index)
            }
            Self::WordLeft => {
                let mut graphemes = text[..cursor]
                    .grapheme_indices(true)
                    .rev()
                    .skip_while(|(_, grapheme)| class(grapheme) == 0)
                    .peekable();
                let Some(word) = graphemes.peek().map(|(_, grapheme)| class(grapheme)) else {
                    return 0;
                };
                graphemes
                    .take_while(|(_, grapheme)| class(grapheme) == word)
                    .last()
                    .map_or(0, |(index, _)| index)
            }
        }
    }
}

pub fn register_app_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    crate::app::windows::commands::register_commands(registry)?;
    crate::app::workspace::commands::register_commands(registry)?;
    crate::app::git::commands::register_commands(registry)?;
    crate::app::diagnostics::register_commands(registry)?;
    crate::app::picker::commands::register_commands(registry)?;
    crate::app::tools::commands::register_commands(registry)?;
    crate::app::plugins::commands::register_commands(registry)?;
    crate::app::terminal::commands::register_commands(registry)?;
    crate::app::editing::commands::register_commands(registry)?;
    crate::app::documents::commands::register_commands(registry)?;
    crate::app::language::commands::register_commands(registry)?;
    crate::app::configuration::commands::register_commands(registry)?;
    crate::app::settings::commands::register_commands(registry)?;
    crate::app::persistence::commands::register_commands(registry)?;
    crate::app::debugger::commands::register_commands(registry)?;
    crate::app::tutor::register_commands(registry)?;
    crate::app::help::register_commands(registry)?;
    crate::app::health::register_commands(registry)?;
    crate::app::tasks::commands::register_commands(registry)?;
    crate::app::search::commands::register_commands(registry)?;
    crate::app::explorer::commands::register_commands(registry)?;
    crate::app::agent::commands::register_commands(registry)?;
    Ok(())
}

pub(in crate::app) fn register_grammar_commands(
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
            "input.character",
            "Complete pending character input",
            GrammarInput::Character,
        ),
        (
            "input.find.begin",
            "Find character on line",
            GrammarInput::FindBegin,
        ),
        (
            "input.mark.begin",
            "Set or jump to a mark",
            GrammarInput::MarkBegin,
        ),
        (
            "input.surround.begin",
            "Delete or replace surrounding pair",
            GrammarInput::SurroundBegin,
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
    for (id, title, reverse) in [
        ("input.find.repeat", "Repeat last find", false),
        ("input.find.reverse", "Repeat last find backward", true),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::View,
            ArgumentSchema::default(),
            move |_| AppEffect::Grammar(GrammarInput::FindRepeat { reverse }),
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

pub(super) fn register<F, R>(
    registry: &mut CommandRegistry<AppEffect>,
    id: &str,
    title: &str,
    context: ContextRequirement,
    arguments: ArgumentSchema,
    effect: F,
) -> Result<(), CommandError>
where
    F: Fn(&crate::editor::CommandInvocation) -> R + Send + Sync + 'static,
    R: IntoEffectResult,
{
    let mut spec = CommandSpec::builtin(id, title)?;
    spec.context = context;
    spec.arguments = arguments;
    use crate::editor::CommandAccess;
    spec.access = [match id {
        "application.notification.dismiss" => CommandAccess::Ui,
        "file.save" | "file.save_all" => CommandAccess::Files,
        "terminal.open" | "task.run" | "format.run" | "lint.run" | "test.run" => {
            CommandAccess::Process
        }
        _ => CommandAccess::HostOnly,
    }]
    .into();
    registry.register(spec, move |_, invocation| {
        let effect = effect(invocation).into_effect_result()?;
        let mut outcome = CommandOutcome::empty();
        outcome.effects.push(effect);
        Ok(outcome)
    })
}

pub(super) trait IntoEffectResult {
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

pub(super) fn string_arg<'a>(
    invocation: &'a crate::editor::CommandInvocation,
    name: &str,
) -> Result<&'a str, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_str()
}

pub(in crate::app) fn optional_string_arg<'a>(
    invocation: &'a crate::editor::CommandInvocation,
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

pub(in crate::app) fn optional_bool_arg(
    invocation: &crate::editor::CommandInvocation,
    name: &str,
) -> Option<bool> {
    invocation
        .arguments
        .get(name)
        .and_then(|value| match value {
            CommandValue::Bool(value) => Some(*value),
            _ => None,
        })
}

pub(in crate::app) fn one_character_arg(
    invocation: &crate::editor::CommandInvocation,
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

pub(in crate::app) fn optional_integer_arg(
    invocation: &crate::editor::CommandInvocation,
    name: &str,
) -> Option<i64> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

pub(super) fn integer_arg(
    invocation: &crate::editor::CommandInvocation,
    name: &str,
) -> Result<i64, CommandError> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Integer(value)) => Ok(*value),
        _ => Err(CommandError::InvalidArguments(format!(
            "missing integer `{name}`"
        ))),
    }
}

pub(in crate::app) fn bool_arg(
    invocation: &crate::editor::CommandInvocation,
    name: &str,
) -> Result<bool, CommandError> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Bool(value)) => Ok(*value),
        _ => Err(CommandError::InvalidArguments(format!(
            "missing boolean `{name}`"
        ))),
    }
}

use crate::app::App;
use crate::editor::CommandInvocation;
use crate::editor::EditorMode;

// Remaining editor/window routing; terminal data and algorithms live in the owner.

impl App {
    pub(in crate::app) fn process_effect(&mut self, effect: AppEffect) {
        self.process_effect_checked(effect);
    }
}

impl App {
    pub(in crate::app) fn process_effect_checked(&mut self, effect: AppEffect) -> bool {
        if let AppEffect::Authorized { authority, effect } = effect {
            let authority = self.effect_authority.restrict(&authority);
            let previous = std::mem::replace(&mut self.effect_authority, authority);
            let failed = std::mem::replace(&mut self.effect_failed, false);
            let succeeded = self.process_effect_checked(*effect) && !self.effect_failed;
            self.effect_failed = failed;
            self.effect_authority = previous;
            return succeeded;
        }
        // Only effects whose owners preserve authority may cross this boundary. Other
        // features remain host-only until their asynchronous requests carry it too.
        use crate::editor::CommandAccess;
        let access = match &effect {
            AppEffect::Save {
                quit_after: false, ..
            }
            | AppEffect::SaveAll { quit_after: false }
            | AppEffect::TerminalOpen(..)
            | AppEffect::TaskRun(..)
            | AppEffect::FormatRun(..)
            | AppEffect::LintRun(..)
            | AppEffect::TestRun(..) => None, // Checked by the final write or run_trusted.
            AppEffect::Input(
                crate::app::input::InputCommand::EnterMode(..)
                | crate::app::input::InputCommand::LeaveMode
                | crate::app::input::InputCommand::PushLayer(..)
                | crate::app::input::InputCommand::PopLayer
                | crate::app::input::InputCommand::PushNamedLayer { .. }
                | crate::app::input::InputCommand::RemoveNamedLayer { .. },
            ) => Some(CommandAccess::Ui),
            AppEffect::Input(crate::app::input::InputCommand::Wait(..))
            | AppEffect::PluginCall { .. } => None,
            AppEffect::LspIncoming(_) => Some(CommandAccess::Edit),
            AppEffect::NotificationDismiss | AppEffect::NotificationHistory => {
                Some(CommandAccess::Ui)
            }
            _ => Some(CommandAccess::HostOnly),
        };
        if let Some(access) = access
            && !self.require_effect_access(access)
        {
            return false;
        }
        let effect = match effect {
            AppEffect::SearchRepeat { reverse, count } => {
                return self.perform_search(self.input.search_direction(reverse), count);
            }
            AppEffect::SearchWord { forward, count } => {
                return self.search_word_under_cursor(forward, count);
            }
            AppEffect::SearchSelectNext(count) => return self.select_next_search_match(count),
            AppEffect::MacroReplay { register, count } => {
                self.input.begin_macro_batch();
                return self.replay_macro(register, count);
            }
            effect => effect,
        };
        // Refuse before replacing the save-and-stage intent owned by the running mutation.
        if self.git.mutation_pending()
            && matches!(
                effect,
                AppEffect::GitFileStage
                    | AppEffect::GitFileUnstage
                    | AppEffect::GitHunkStage
                    | AppEffect::GitHunkUnstage
                    | AppEffect::GitLinesStage { .. }
                    | AppEffect::GitConflictStage
                    | AppEffect::GitHunkReset
            )
        {
            self.show_error("a Git mutation is already running");
            return true;
        }
        let result = match effect {
            AppEffect::Authorized { .. } => unreachable!("authority handled above"),
            AppEffect::PluginCall {
                plugin,
                handler,
                call,
                pending,
                previous,
            } => {
                return self.execute_plugin_call(
                    plugin,
                    handler,
                    *call,
                    pending,
                    previous,
                    self.effect_authority.clone(),
                );
            }
            AppEffect::CommandResume {
                pending,
                owner,
                action,
            } => {
                return self.resume_command(pending, &owner, *action);
            }
            effect @ (AppEffect::LspDocumentationScroll(..) | AppEffect::LspDocumentationClose) => {
                self.process_documentation_effect(effect)
            }
            effect @ AppEffect::DiagnosticNavigate { .. } => {
                self.process_diagnostics_effect(effect)
            }
            effect @ (AppEffect::HelpForForm
            | AppEffect::HelpContextual
            | AppEffect::HelpCommandReference) => self.process_help_effect(effect),
            effect @ (AppEffect::CrashInspect
            | AppEffect::CrashClear
            | AppEffect::ShowHealth
            | AppEffect::HealthVirtualOpen
            | AppEffect::HealthVirtualRefresh) => self.process_health_effect(effect),
            effect @ (AppEffect::Tutor
            | AppEffect::TutorNext
            | AppEffect::TutorBack
            | AppEffect::TutorQuit) => self.process_tutor_effect(effect),
            effect @ AppEffect::Quit { .. } => self.process_shutdown_effect(effect),
            effect @ (AppEffect::ExplorerPanelToggle
            | AppEffect::ExplorerOpen
            | AppEffect::ExplorerSearch
            | AppEffect::ExplorerSearchInsert(..)
            | AppEffect::ExplorerSearchDelete
            | AppEffect::ExplorerSearchStop { .. }
            | AppEffect::ExplorerClose
            | AppEffect::ExplorerMove(..)
            | AppEffect::ExplorerPage(..)
            | AppEffect::ExplorerParent
            | AppEffect::ExplorerChild
            | AppEffect::ExplorerToggle
            | AppEffect::ExplorerRefresh
            | AppEffect::ExplorerFollow
            | AppEffect::ExplorerToggleHidden
            | AppEffect::ExplorerToggleIgnored
            | AppEffect::ExplorerFileOpen { .. }
            | AppEffect::ExplorerCreate(..)
            | AppEffect::ExplorerRename(..)
            | AppEffect::ExplorerRenamePrompt
            | AppEffect::ExplorerDeleteRequest
            | AppEffect::ExplorerClipboard { .. }
            | AppEffect::ExplorerPaste
            | AppEffect::ExplorerYankPath
            | AppEffect::ExplorerMouseSelect) => self.process_explorer_effect(effect),
            effect @ AppEffect::Agent(..) => self.process_agent_effect(effect),
            effect @ (AppEffect::WorkspaceTrust
            | AppEffect::WorkspaceUntrust
            | AppEffect::WorkspaceStatus
            | AppEffect::WorkspaceRootAdd(..)
            | AppEffect::WorkspaceRootActivate(..)
            | AppEffect::WorkspaceRootRemove(..)
            | AppEffect::WorkspaceRootStatus
            | AppEffect::SafeModeStatus) => self.process_workspace_effect(effect),
            effect @ (AppEffect::WorkspaceSearchPrompt
            | AppEffect::WorkspaceSearch(..)
            | AppEffect::WorkspaceSearchCancel
            | AppEffect::WorkspaceSearchResume
            | AppEffect::WorkspaceReplace { .. }
            | AppEffect::TodoSearch
            | AppEffect::SearchRepeat { .. }
            | AppEffect::SearchWord { .. }
            | AppEffect::SearchSelectNext(..)) => self.process_search_effect(effect),
            effect @ (AppEffect::WorkspaceEditToggle
            | AppEffect::WorkspaceEditAccept
            | AppEffect::WorkspaceEditReject
            | AppEffect::WorkspaceResourceApply) => self.process_workspace_edits_effect(effect),
            effect @ (AppEffect::Save { .. }
            | AppEffect::SaveAll { .. }
            | AppEffect::OpenPath(..)
            | AppEffect::ForceOpenPath(..)
            | AppEffect::OpenUserFile(..)
            | AppEffect::Reload { .. }
            | AppEffect::CloseBuffer { .. }
            | AppEffect::CycleBuffer(..)
            | AppEffect::ToggleBufferPin
            | AppEffect::MoveBufferLine(..)
            | AppEffect::NewBuffer
            | AppEffect::LargeFileSet(..)
            | AppEffect::LargeFileStatus
            | AppEffect::WalkJumps(..)
            | AppEffect::RecoveryInspect
            | AppEffect::MarkdownPreview) => self.process_documents_effect(effect),
            effect @ (AppEffect::ReloadConfiguration
            | AppEffect::Zen
            | AppEffect::SelectTheme
            | AppEffect::SetTheme(..)
            | AppEffect::ViewToggle(..)
            | AppEffect::ExplainConfig(..)
            | AppEffect::InspectKeymap { .. }
            | AppEffect::ShowKeymapConflicts) => self.process_configuration_effect(effect),
            effect @ (AppEffect::DashboardSelect(..) | AppEffect::DashboardOpen) => {
                self.process_dashboard_effect(effect)
            }
            effect @ (AppEffect::PackageInstall { .. }
            | AppEffect::PackageRegistrySearch(..)
            | AppEffect::PackageRegistryShow { .. }
            | AppEffect::PackageRegistryInstall { .. }
            | AppEffect::PackageList
            | AppEffect::PackageVerify
            | AppEffect::PackageRollback
            | AppEffect::PackageRemove(..)
            | AppEffect::PackageGarbageCollect
            | AppEffect::OpenToolMenu(..)
            | AppEffect::CloseToolMenu
            | AppEffect::ToolSearch
            | AppEffect::ToolSearchInsert(..)
            | AppEffect::ToolSearchDelete
            | AppEffect::ToolSearchStop { .. }
            | AppEffect::MoveToolMenu(..)
            | AppEffect::CycleToolCategory(..)
            | AppEffect::RefreshTools
            | AppEffect::ActivateTool
            | AppEffect::InstallSelectedTool
            | AppEffect::UninstallSelectedTool
            | AppEffect::StopSelectedLsp
            | AppEffect::RestartSelectedLsp
            | AppEffect::ToggleToolRuntime) => self.process_tools_effect(effect),
            effect @ (AppEffect::TestRun(..)
            | AppEffect::TestStop(..)
            | AppEffect::TestList(..)
            | AppEffect::TaskList
            | AppEffect::TaskRestart(..)
            | AppEffect::TaskRun(..)
            | AppEffect::TaskStop(..)
            | AppEffect::TaskOutput(..)
            | AppEffect::LintRun(..)
            | AppEffect::LintClear(..)
            | AppEffect::FormatRun(..)) => self.process_tasks_effect(effect),
            effect @ (AppEffect::DebuggerBreakpointStates
            | AppEffect::DebuggerBreakpointDisable
            | AppEffect::DebuggerTerminate
            | AppEffect::DebuggerDetach
            | AppEffect::DapStart(..)
            | AppEffect::DapCursorTest
            | AppEffect::DapConfigurationStart(..)
            | AppEffect::DapWatchAdd(..)
            | AppEffect::DapWatchRemove(..)
            | AppEffect::DapWatchList
            | AppEffect::DapStop
            | AppEffect::DapContinue
            | AppEffect::DapPause
            | AppEffect::DapNext
            | AppEffect::DapStepIn
            | AppEffect::DapStepOut
            | AppEffect::DapConsole
            | AppEffect::DapSessions
            | AppEffect::DapThreads
            | AppEffect::DapStack(..)
            | AppEffect::DapScopes(..)
            | AppEffect::DapVariables(..)
            | AppEffect::DapEvaluate(..)
            | AppEffect::DapRestart
            | AppEffect::DapBreakpointConfigure { .. }
            | AppEffect::DapBreakpointToggle
            | AppEffect::DapInspectorNext
            | AppEffect::DapStatus) => self.process_debugger_effect(effect),
            effect @ (AppEffect::GitFileStage
            | AppEffect::GitFileUnstage
            | AppEffect::GitLinesStage { .. }
            | AppEffect::GitWorktreeCreate { .. }
            | AppEffect::GitConflictsOpen
            | AppEffect::GitConflictAccept { .. }
            | AppEffect::GitConflictChoose { .. }
            | AppEffect::GitConflictStage
            | AppEffect::GitConflictContinue
            | AppEffect::GitConflictAbort
            | AppEffect::GitConflictReplace { .. }
            | AppEffect::GitWorktreeOpen
            | AppEffect::GitWorktreeRemove
            | AppEffect::GitWorktreesOpen
            | AppEffect::GitHunksOpenStaged
            | AppEffect::GitHunkUnstage
            | AppEffect::GitStatusRefresh
            | AppEffect::GitStatusOpen
            | AppEffect::GitBrowse { .. }
            | AppEffect::PullRequestList
            | AppEffect::PullRequestComments
            | AppEffect::PullRequestComment
            | AppEffect::GitChangesLayerSwitch
            | AppEffect::GitHunksOpen
            | AppEffect::GitConflictNavigate(..)
            | AppEffect::GitHunkNavigate(..)
            | AppEffect::GitReviewHunk(..)
            | AppEffect::GitHunkPreview
            | AppEffect::GitHunkStage
            | AppEffect::GitHunkReset) => self.process_git_effect(effect),
            effect @ (AppEffect::PluginValidate
            | AppEffect::PluginList
            | AppEffect::PluginDisable(..)
            | AppEffect::PluginPickerOpen { .. }
            | AppEffect::PluginExecute { .. }
            | AppEffect::RetainedUiFocus(..)
            | AppEffect::RetainedUiScroll(..)
            | AppEffect::RetainedUiActivate
            | AppEffect::RetainedUiMouseSelect
            | AppEffect::PrivateUiActivate { .. }
            | AppEffect::VirtualDocumentOpen { .. }
            | AppEffect::VirtualDocumentRefresh) => self.process_plugins_effect(effect),
            effect @ (AppEffect::LspStart
            | AppEffect::LspStop
            | AppEffect::LspRestart
            | AppEffect::LspStatus
            | AppEffect::LspAttach
            | AppEffect::LspDetach
            | AppEffect::LspSessions
            | AppEffect::LspSessionSelect(..)
            | AppEffect::LspLogs
            | AppEffect::LspInlayHintsToggle
            | AppEffect::LspRequest(..)
            | AppEffect::LspCompletionDocumentationOpen
            | AppEffect::LspCompletionMove(..)
            | AppEffect::LspCompletionAccept
            | AppEffect::LspCompletionInlineAccept
            | AppEffect::LspIncoming(..)
            | AppEffect::LspRenamePrompt
            | AppEffect::LspCompletionLeave
            | AppEffect::LspCompletionCancel
            | AppEffect::SnippetNavigate(..)
            | AppEffect::SnippetLeave
            | AppEffect::SnippetCancel
            | AppEffect::LspFoldToggle
            | AppEffect::LspSelectionExpand) => self.process_language_effect(effect),
            effect @ (AppEffect::Settings(..) | AppEffect::Setup(..)) => {
                self.process_settings_effect(effect)
            }
            effect @ (AppEffect::TerminalOpen(..)
            | AppEffect::TerminalToggle
            | AppEffect::TerminalList
            | AppEffect::TerminalFocus(..)
            | AppEffect::TerminalFocusActive
            | AppEffect::TerminalCycle(..)
            | AppEffect::TerminalResize(..)
            | AppEffect::TerminalDock { .. }
            | AppEffect::TerminalFloat(..)
            | AppEffect::TerminalFullscreen
            | AppEffect::TerminalHide
            | AppEffect::TerminalUnfocus
            | AppEffect::TerminalSend(..)
            | AppEffect::TerminalModeNormal
            | AppEffect::TerminalModeInput
            | AppEffect::TerminalNormalMove(..)
            | AppEffect::TerminalVisualStart(..)
            | AppEffect::TerminalYank { .. }
            | AppEffect::TerminalClose
            | AppEffect::TerminalQuit) => self.process_terminal_effect(effect),
            effect @ (AppEffect::OpenWorkspaceFilePicker
            | AppEffect::OpenPicker(..)
            | AppEffect::PickerInsert(..)
            | AppEffect::PickerBackspace
            | AppEffect::PickerMove(..)
            | AppEffect::PickerPreviewPage(..)
            | AppEffect::PickerResults
            | AppEffect::PickerRemove
            | AppEffect::PickerAccept
            | AppEffect::PickerCancel
            | AppEffect::PickerNormal
            | AppEffect::PickerInsertMode) => self.process_picker_effect(effect),
            effect @ (AppEffect::FlashBegin
            | AppEffect::SubstituteAll
            | AppEffect::Scroll { .. }
            | AppEffect::RepeatLast(..)
            | AppEffect::MacroRecordStart(..)
            | AppEffect::MacroRecordStop
            | AppEffect::MacroReplay { .. }
            | AppEffect::Grammar(..)
            | AppEffect::Substitute { .. }
            | AppEffect::GlobalDelete { .. }) => self.process_editing_effect(effect),
            effect @ (AppEffect::OpenPrompt { .. }
            | AppEffect::OpenFilePrompt
            | AppEffect::PromptInsert(..)
            | AppEffect::PromptBackspace
            | AppEffect::PromptDeleteWord
            | AppEffect::PromptDeleteToStart
            | AppEffect::PromptMove(..)
            | AppEffect::PromptHistory(..)
            | AppEffect::PromptComplete(..)
            | AppEffect::PromptConfirmationSelect(..)
            | AppEffect::PromptConfirmationToggle
            | AppEffect::PromptSubmit
            | AppEffect::PromptCancel) => self.process_prompt_effect(effect),
            effect @ (AppEffect::NotificationHistory | AppEffect::NotificationDismiss) => {
                self.process_notifications_effect(effect)
            }
            effect @ (AppEffect::Input(..)
            | AppEffect::PlaceMouseCursor
            | AppEffect::ExtendMouseSelection
            | AppEffect::CancelPendingMapping) => self.process_input_effect(effect),
            effect @ (AppEffect::HistoryClear
            | AppEffect::SessionSave
            | AppEffect::SessionLoad
            | AppEffect::MarkJump(..)
            | AppEffect::MarkSet(..)) => self.process_persistence_effect(effect),
            effect @ (AppEffect::SyntaxFold(..)
            | AppEffect::SyntaxIndentLine
            | AppEffect::SyntaxStatus) => self.process_syntax_effect(effect),
            effect @ (AppEffect::NewTab
            | AppEffect::Layout(..)
            | AppEffect::SurfaceFocus(..)
            | AppEffect::WindowClone { .. }
            | AppEffect::WindowList
            | AppEffect::WindowDock { .. }
            | AppEffect::WindowFloat { .. }
            | AppEffect::WindowFullscreen(..)
            | AppEffect::WindowHide(..)
            | AppEffect::WindowShow(..)
            | AppEffect::WindowClose(..)
            | AppEffect::WindowResize { .. }
            | AppEffect::WindowResizeDirectional(..)
            | AppEffect::WindowMove { .. }
            | AppEffect::WindowSwap { .. }) => self.process_windows_effect(effect),
        };
        match result {
            Ok(()) => true,
            Err(_) => unreachable!("every AppEffect variant has one owning handler"),
        }
    }
}

impl App {
    pub(in crate::app) fn invoke(&mut self, invocation: CommandInvocation) {
        self.invoke_checked(invocation);
    }
}

#[derive(Clone)]
pub(in crate::app) struct PendingInvocation {
    pub(in crate::app) invocation: CommandInvocation,
    pub(in crate::app) repeat_override: Option<CommandInvocation>,
    pub(in crate::app) suppress_repeat: bool,
}

impl App {
    pub(in crate::app) fn invoke_checked(&mut self, invocation: CommandInvocation) -> bool {
        let authority = if invocation.source == crate::editor::InvocationSource::Plugin {
            CommandAuthority::Plugin {
                initiator: String::new(),
                grants: Default::default(),
            }
        } else {
            CommandAuthority::Host
        };
        self.invoke_with_authority(invocation, authority)
    }

    pub(in crate::app) fn invoke_with_authority(
        &mut self,
        mut invocation: CommandInvocation,
        authority: CommandAuthority,
    ) -> bool {
        let authority = self.effect_authority.restrict(&authority);
        // Keys typed while a command is pending wait for it; only host input is held,
        // because a held invocation is replayed without the caller's grants.
        if authority == CommandAuthority::Host
            && !invocation.id.as_str().starts_with("lsp.incoming.")
            && self.input.hold_invocation(&invocation)
        {
            return true;
        }
        if let Err(error) = self.commands.check_access(&invocation.id, &authority) {
            self.show_error(error);
            return false;
        }
        // Clipboard deferral currently stores native input, not a caller's grants.
        if authority != CommandAuthority::Host && self.clipboard.waiting() {
            self.show_error("clipboard input is pending");
            return false;
        }
        if self.clipboard.waiting() {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Invocation(
                    invocation,
                ));
            return true;
        }
        // Background server notifications are not user actions that dismiss documentation.
        if !invocation.id.as_str().starts_with("lsp.documentation.")
            && !invocation.id.as_str().starts_with("lsp.incoming.")
        {
            self.close_documentation();
        }
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view();
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
        }
        if authority == CommandAuthority::Host
            && invocation.id.as_str() == "edit.put"
            && matches!(invocation.register, None | Some('"' | '+' | '*'))
            && (self.clipboard_shared() || matches!(invocation.register, Some('+' | '*')))
            && {
                let shared = self.clipboard_shared();
                self.clipboard.pull(
                    &invocation,
                    self.state.window_version(),
                    &mut self.state.value.registers,
                    shared,
                )
            }
        {
            return true;
        }
        if invocation.id.as_str() == "edit.operator"
            && invocation.arguments.get("operator") == Some(&CommandValue::String("comment".into()))
            && !invocation.arguments.contains_key("comment")
        {
            let token = invocation
                .target
                .buffer
                .and_then(|id| self.state.buffers.get(id))
                .and_then(|buffer| buffer.path.as_deref())
                .and_then(|path| path.extension())
                .and_then(|extension| {
                    self.configuration
                        .settings()
                        .editor
                        .comments
                        .get(extension.to_string_lossy().as_ref())
                        .cloned()
                });
            if let Some(token) = token {
                invocation = invocation.with_argument("comment", CommandValue::String(token));
            }
        }
        // `cgn`, `dgn`: the operator acts on the next search match, selected as `gn` does, and
        // `.` repeats it on the match after that.
        let search_match = (invocation.id.as_str() == "edit.operator"
            && invocation.arguments.get("motion")
                == Some(&CommandValue::String("search_match".into())))
        .then(|| invocation.clone());
        if search_match.is_some() {
            if !self.select_next_search_match(1) {
                self.set_mode(EditorMode::Normal);
                return false;
            }
            invocation = invocation.with_argument("motion", CommandValue::String("visual".into()));
        }
        let visual_operator = invocation.id.as_str() == "edit.operator"
            && invocation
                .arguments
                .get("motion")
                .is_some_and(|motion| motion == &CommandValue::String("visual".into()));
        let suppress_repeat = visual_operator
            && invocation
                .arguments
                .get("operator")
                .is_some_and(|operator| operator == &CommandValue::String("yank".into()));
        let repeat_override = search_match.or_else(|| {
            visual_operator
                .then(|| self.normalized_visual_repeat(&invocation))
                .flatten()
        });
        // Long moves are jumps that `ctrl-o` comes back from, as in Vim.
        if invocation.id.as_str() == "view.cursor.move"
            && invocation.arguments.get("motion").is_some_and(|motion| {
                matches!(
                    motion,
                    CommandValue::String(motion) if matches!(
                        motion.as_str(),
                        "document_start" | "document_end" | "line_number" | "matching_pair"
                            | "paragraph_forward" | "paragraph_backward"
                    )
                )
            })
        {
            self.record_jump();
        }
        if invocation.id.as_str() == "edit.insert_text"
            && let Some(CommandValue::String(text)) = invocation.arguments.get("text")
        {
            self.language.note_typed(text);
        }
        let outcome = self
            .commands
            .execute_with_authority(&self.state, &invocation, authority);
        self.finish_command(
            PendingInvocation {
                invocation,
                repeat_override,
                suppress_repeat,
            },
            outcome,
        )
    }

    pub(in crate::app) fn cancel_plugin_pending_command(&mut self, pending: PendingCommandId) {
        self.commands.cancel(pending);
        self.plugin_command_resuming(pending);
        self.plugin_command_result(None, Some("plugin command cancelled".into()));
        self.complete_input_command(pending, false, false);
        self.finish_incoming_lsp_commands();
    }

    pub(in crate::app) fn incoming_command_pending(&self, id: i64) -> bool {
        self.input.incoming_command_pending(id, &self.commands)
    }

    fn resume_command(
        &mut self,
        pending: PendingCommandId,
        owner: &str,
        action: CommandWrapperAction<AppEffect>,
    ) -> bool {
        let outcome = self.commands.resume(&self.state, pending, owner, action);
        // An unrelated caller cannot consume the real wrapper's continuation.
        if matches!(outcome, Err(CommandError::WrongWrapper)) {
            self.show_error(CommandError::WrongWrapper);
            return false;
        }
        let Some(invocation) = self.input.pending_invocation(pending) else {
            self.commands.cancel(pending);
            self.show_error(CommandError::UnknownPending);
            return false;
        };
        self.plugin_command_resuming(pending);
        let waiting = outcome
            .as_ref()
            .is_ok_and(|outcome| outcome.pending.is_some());
        let buffer = invocation
            .invocation
            .target
            .buffer
            .or_else(|| self.state.active_buffer_id());
        let before = buffer.and_then(|id| {
            self.state
                .buffers
                .get(id)
                .map(|buffer| buffer.text.source_version())
        });
        let succeeded = self.finish_command(invocation, outcome);
        let changed = before
            != buffer.and_then(|id| {
                self.state
                    .buffers
                    .get(id)
                    .map(|buffer| buffer.text.source_version())
            });
        if !succeeded {
            self.plugin_command_result(None, Some("plugin command failed".into()));
        }
        if !waiting || !succeeded {
            self.complete_input_command(pending, succeeded, changed);
        }
        self.finish_incoming_lsp_commands();
        succeeded
    }

    pub(in crate::app) fn require_effect_access(
        &mut self,
        access: crate::editor::CommandAccess,
    ) -> bool {
        if let Err(error) = self.effect_authority.require(access) {
            self.effect_failed = true;
            self.show_error(error);
            return false;
        }
        true
    }

    fn finish_command(
        &mut self,
        invocation: PendingInvocation,
        outcome: Result<CommandOutcome<AppEffect>, CommandError>,
    ) -> bool {
        let PendingInvocation {
            invocation,
            repeat_override,
            suppress_repeat,
        } = invocation;
        match outcome {
            Ok(outcome) => {
                if let Some(pending) = outcome.pending {
                    self.plugin_command_pending(pending);
                    self.input.suspend_command(
                        pending,
                        PendingInvocation {
                            invocation,
                            repeat_override,
                            suppress_repeat,
                        },
                    );
                    for effect in outcome.effects {
                        if !self.process_effect_checked(AppEffect::Authorized {
                            authority: outcome.authority.clone(),
                            effect: Box::new(effect),
                        }) {
                            self.cancel_plugin_pending_command(pending);
                            return false;
                        }
                    }
                    return true;
                }
                let text_before = invocation.target.buffer.and_then(|buffer| {
                    self.state
                        .buffers
                        .get(buffer)
                        .map(|buffer| buffer.text.source_version())
                });
                if let Some(mut transaction) = outcome.transaction {
                    if let Err(error) = outcome
                        .authority
                        .require(crate::editor::CommandAccess::Edit)
                    {
                        self.show_error(error);
                        return false;
                    }
                    self.persistence.reconcile_marks(&self.state);
                    let source = self.state.window_version();
                    let snippet_document = self.input.snippet_buffer().and_then(|id| {
                        self.state.buffers.get(id).map(|buffer| {
                            crate::app::language::snippets::SnippetDocument {
                                editor: &source,
                                buffer: id,
                                text: &buffer.text,
                                view: None,
                            }
                        })
                    });
                    let update = self
                        .input
                        .prepare_snippet_mirror(snippet_document, &mut transaction);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                        return false;
                    }
                    self.highlight_yank(&transaction);
                    // Map marks before a long typing session or macro evicts the edit history.
                    self.persistence.reconcile_marks(&self.state);
                    if let Some(update) = update {
                        let source = self.state.window_version();
                        let document = self.input.snippet_buffer().and_then(|id| {
                            self.state.buffers.get(id).map(|buffer| {
                                crate::app::language::snippets::SnippetDocument {
                                    editor: &source,
                                    buffer: id,
                                    text: &buffer.text,
                                    view: None,
                                }
                            })
                        });
                        self.input.accept_snippet_edit(update, document);
                    }
                }

                self.keep_cursor_off_final_newline();
                let text_changed = invocation.target.buffer.and_then(|buffer| {
                    self.state
                        .buffers
                        .get(buffer)
                        .map(|buffer| buffer.text.source_version())
                }) != text_before;
                if text_changed {
                    self.push_change_location();
                }
                // Submitting the command prompt closes it and invokes the semantic command.
                // Do not record its UI keystroke as a second macro step after it closes.
                let command_prompt = invocation.id.as_str().starts_with("command_line.")
                    && self
                        .input
                        .prompt()
                        .is_some_and(|prompt| prompt.kind == PromptKind::Command);
                // Establish the hold before effects can complete synchronously, but do not
                // record a command whose final operation is refused.
                let waiting = self.input.wait_for_effects(
                    &invocation,
                    repeat_override.as_ref(),
                    suppress_repeat,
                    text_changed,
                    &outcome.effects,
                );
                for effect in outcome.effects {
                    if !self.process_effect_checked(AppEffect::Authorized {
                        authority: outcome.authority.clone(),
                        effect: Box::new(effect),
                    }) {
                        self.fail_pending_input_command();
                        return false;
                    }
                }
                for follow_up in outcome.follow_up {
                    if !self.invoke_with_authority(follow_up, outcome.authority.clone()) {
                        self.fail_pending_input_command();
                        return false;
                    }
                }
                if !waiting
                    && !command_prompt
                    && !invocation.id.as_str().starts_with("lsp.incoming.")
                {
                    self.input.record_command_outcome(
                        &invocation,
                        repeat_override.as_ref(),
                        suppress_repeat,
                        text_changed,
                    );
                }
                self.tutor_observe(&invocation);
                self.sync_flash_ui();
                self.plugin_command_result(outcome.value.as_ref(), None);
                true
            }
            Err(error) => {
                self.plugin_command_result(None, Some(error.to_string()));
                // A failed `dfz` or `di(` must not leave the view stuck in operator-pending.
                if self
                    .state
                    .active_view()
                    .and_then(|view| self.state.views.get(view))
                    .is_some_and(|view| view.mode == EditorMode::OperatorPending)
                {
                    self.set_mode(EditorMode::Normal);
                }
                self.show_error(error);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::commands::PromptMove;

    #[test]
    fn suspended_builtin_applies_only_when_its_wrapper_continues() {
        use crate::editor::{CommandAuthority, CommandWrapperAction};
        use crate::editor::{CommandInvocation, CommandValue, InvocationSource};
        let (_dirs, mut app) = crate::app::tests::app();
        let (buffer, _) = app
            .state
            .open_text(crate::editor::text::TextBuffer::from_text(""), None)
            .unwrap();
        let invocation = CommandInvocation::new("edit.insert_text", InvocationSource::Test)
            .unwrap()
            .with_argument("text", CommandValue::String("wrapped".into()));
        app.commands
            .register_wrapper(
                &invocation.id,
                "example".into(),
                0,
                CommandAuthority::Host,
                |_, _, _, result| {
                    Ok(match result {
                        None => CommandWrapperAction::Pending(vec![]),
                        Some(result) => CommandWrapperAction::Complete(result),
                    })
                },
            )
            .unwrap();
        assert!(app.invoke_checked(invocation.clone()));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "");
        let pending = app.input.pending_input_command().unwrap();
        assert!(
            !app.process_effect_checked(super::AppEffect::CommandResume {
                pending,
                owner: "other".into(),
                action: Box::new(CommandWrapperAction::CallNext(invocation.clone())),
            })
        );
        assert_eq!(app.input.pending_input_command(), Some(pending));
        assert!(app.process_effect_checked(super::AppEffect::CommandResume {
            pending,
            owner: "example".into(),
            action: Box::new(CommandWrapperAction::CallNext(invocation)),
        }));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "wrapped");
        assert!(!app.input.command_pending());
    }

    #[test]
    fn restricted_wrapper_cannot_emit_host_application_effects() {
        use crate::editor::{CommandAccess, CommandAuthority, CommandWrapperAction};
        use crate::editor::{CommandInvocation, CommandOutcome, CommandValue, InvocationSource};
        let (_dirs, mut app) = crate::app::tests::app();
        let (buffer, _) = app
            .state
            .open_text(crate::editor::text::TextBuffer::from_text(""), None)
            .unwrap();
        let invocation = CommandInvocation::new("edit.insert_text", InvocationSource::Test)
            .unwrap()
            .with_argument("text", CommandValue::String("no".into()));
        app.commands
            .register_wrapper(
                &invocation.id,
                "example".into(),
                0,
                CommandAuthority::Plugin {
                    initiator: "example".into(),
                    grants: [CommandAccess::Edit].into(),
                },
                |_, _, _, _| {
                    Ok(CommandWrapperAction::Complete(CommandOutcome {
                        effects: vec![super::AppEffect::Quit { force: true }],
                        ..CommandOutcome::empty()
                    }))
                },
            )
            .unwrap();
        assert!(!app.invoke_checked(invocation));
        assert!(!app.should_quit);
        assert_eq!(app.state.buffers[buffer].text.to_string(), "");
    }

    #[test]
    fn plugin_deadline_does_not_unregister_commands_or_unload_the_plugin() {
        use crate::editor::{
            CommandInvocation, CommandOutcome, CommandSource, CommandSpec, InvocationSource,
        };
        let (_dirs, mut app) = crate::app::tests::app();
        let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
        app.extension.fixture_manager(manager);
        let mut spec = CommandSpec::builtin("plugin.demo.run", "Demo").unwrap();
        spec.source = CommandSource::Plugin("demo".into());
        app.commands
            .register(spec, |_, _| Ok(CommandOutcome::empty()))
            .unwrap();
        fixture.command_failed(
            "demo",
            crate::plugins::PluginLoadId(1),
            crate::plugins::PluginError::Deadline,
        );
        app.process_plugin_events();
        assert!(
            app.extension
                .is_active_load("demo", crate::plugins::PluginLoadId(1))
        );
        assert!(app.invoke_checked(
            CommandInvocation::new("plugin.demo.run", InvocationSource::Test).unwrap()
        ));
    }

    #[test]
    fn plugin_effect_is_refused_at_the_final_write_not_just_command_dispatch() {
        use crate::editor::{
            CommandAccess, CommandAuthority, CommandInvocation, CommandOutcome, CommandSource,
            CommandSpec, InvocationSource,
        };
        let (_dirs, mut app) = crate::app::tests::app();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output.txt");
        app.state
            .open_text(
                crate::editor::text::TextBuffer::from_text("plugin text"),
                None,
            )
            .unwrap();
        let mut spec = CommandSpec::builtin("plugin.writer.save", "Write").unwrap();
        spec.source = CommandSource::Plugin("writer".into());
        spec.access = [CommandAccess::Edit].into();
        let destination = path.clone();
        app.commands
            .register(spec, move |_, _| {
                Ok(CommandOutcome {
                    effects: vec![super::AppEffect::Save {
                        quit_after: false,
                        path: Some(destination.clone()),
                        force: false,
                    }],
                    ..CommandOutcome::empty()
                })
            })
            .unwrap();
        let invocation =
            CommandInvocation::new("plugin.writer.save", InvocationSource::Plugin).unwrap();
        let authority = CommandAuthority::Plugin {
            initiator: "writer".into(),
            grants: [CommandAccess::Edit].into(),
        };
        assert!(
            app.commands
                .check_access(&invocation.id, &authority)
                .is_ok()
        );
        assert!(!app.invoke_with_authority(invocation.clone(), authority));
        assert!(!path.exists());
        assert!(!app.documents.file_work_pending());
        let authority = CommandAuthority::Plugin {
            initiator: "writer".into(),
            grants: [CommandAccess::Edit, CommandAccess::Files].into(),
        };
        assert!(app.invoke_with_authority(invocation, authority));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.documents.file_work_pending() {
            app.process_vfs_results();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "plugin text");
    }

    #[test]
    fn delegated_effect_cannot_widen_the_initiators_file_or_process_grants() {
        use crate::editor::{CommandAccess, CommandAuthority};
        let (_dirs, mut app) = crate::app::tests::app();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output.txt");
        app.state
            .open_text(crate::editor::text::TextBuffer::from_text("no"), None)
            .unwrap();
        let caller = CommandAuthority::Plugin {
            initiator: "caller".into(),
            grants: [CommandAccess::Edit].into(),
        };
        let delegate = CommandAuthority::Plugin {
            initiator: "delegate".into(),
            grants: [
                CommandAccess::Edit,
                CommandAccess::Files,
                CommandAccess::Process,
            ]
            .into(),
        };
        for effect in [
            super::AppEffect::Save {
                quit_after: false,
                path: Some(path.clone()),
                force: false,
            },
            super::AppEffect::TerminalOpen(None),
        ] {
            assert!(!app.process_effect_checked(super::AppEffect::Authorized {
                authority: caller.clone(),
                effect: Box::new(super::AppEffect::Authorized {
                    authority: delegate.clone(),
                    effect: Box::new(effect)
                }),
            }));
        }
        assert!(!path.exists());
        assert!(!app.documents.file_work_pending());
        assert_eq!(app.effect_authority, CommandAuthority::Host);
    }

    #[test]
    fn word_moves_stop_at_words_and_punctuation_runs_as_vim_does() {
        let text = "git  push --force origin";
        let stops = |movement: PromptMove, from: usize| {
            let mut stops = Vec::new();
            let mut cursor = from;
            loop {
                let next = movement.apply(text, cursor);
                if next == cursor {
                    return stops;
                }
                stops.push(next);
                cursor = next;
            }
        };
        assert_eq!(stops(PromptMove::WordRight, 0), [5, 10, 12, 18, 24]);
        assert_eq!(stops(PromptMove::WordLeft, 24), [18, 12, 10, 5, 0]);
        assert_eq!(PromptMove::WordLeft.apply(text, 7), 5);
        assert_eq!(PromptMove::WordRight.apply("añb c", 0), 5);
        let text = "cafe\u{301} next";
        assert_eq!(PromptMove::WordRight.apply(text, 0), "cafe\u{301} ".len());
        assert_eq!(PromptMove::WordLeft.apply(text, "cafe\u{301}".len()), 0);
        let text = "👩‍💻! next";
        assert_eq!(PromptMove::WordRight.apply(text, 0), "👩‍💻! ".len());
        assert_eq!(PromptMove::WordLeft.apply(text, "👩‍💻!".len()), 0);
        assert_eq!(PromptMove::WordRight.apply("e\u{301}.x", 0), 3);
        assert_eq!(PromptMove::WordLeft.apply("e\u{301}.x", 3), 0);
        assert_eq!(PromptMove::WordLeft.apply("👩‍💻 x", 12), 0);
    }
}
