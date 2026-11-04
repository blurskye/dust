mod agent;
mod configuration;
mod debugger;
mod editing;
mod files;
mod language;
mod packages;
mod project;
mod tasks;
mod terminal;
mod windows;
mod workspace;

use std::path::PathBuf;

use crate::editor::{
    ArgumentSchema, ArgumentType, CommandError, CommandOutcome, CommandRegistry, CommandSpec,
    CommandValue, ContextRequirement, FocusDirection,
};
use crate::{
    editor::input::GrammarInput,
    ui::{ConfirmationChoice, PromptKind},
};

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
    AgentAsk,
    AgentToggle,
    AgentAddFile,
    AgentAddDiagnostics,
    AgentNew,
    AgentSessions,
    AgentNextSession,
    AgentPreviousSession,
    AgentCloseSession,
    AgentResume,
    AgentCancel,
    AgentModel,
    AgentMode,
    AgentThinking,
    AgentOptions,
    AgentInput(String),
    AgentEdit(String),
    AgentScroll(i64),
    AgentEscape,
    AgentNewTab,
    NewTab,
    PullRequestList,
    PullRequestComments,
    PullRequestComment,
    /// Switch the change surface's diff pane between the unstaged and staged comparison.
    GitChangesLayerSwitch,
    GitHunksOpen,
    GitHunkNavigate(i8),
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
    GitConflictReplaceSubmit,
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
    },
    SearchSelectNext,
    SubstituteAll,
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
    /// Moves the dashboard cursor by this many items.
    DashboardSelect(i8),
    DashboardOpen,
    WindowList,
    WindowClone {
        target: String,
        axis: crate::editor::Axis,
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
    TerminalFloat(Option<crate::ui::window::WindowRect>),
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
    ViewToggle(crate::editor::session::ViewToggle),
    Zen,
    LspRequest(crate::language::lsp::LspFeature),
    LspDocumentationScroll(isize),
    LspDocumentationClose,
    LspCompletionDocumentationOpen,
    LspCompletionMove(i8),
    LspCompletionAccept,
    LspCompletionCancel,
    SnippetNavigate(i8),
    SnippetLeave,
    SnippetCancel,
    LspFoldToggle,
    LspSelectionExpand,
    DiagnosticNavigate {
        direction: i8,
        filter: DiagnosticFilter,
    },
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
        use crate::workspace::DiagnosticSeverity::{Error, Warning};
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

pub use crate::editor::session::windows::LayoutAction;

/// What a key on the Settings page does.
#[derive(Clone, Debug)]
pub enum SettingsAction {
    Open,
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
        let class = |character: char| {
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
                let mut characters = text[cursor..].char_indices().peekable();
                let start = characters.peek().map(|(_, character)| class(*character));
                characters
                    .skip_while(|(_, character)| Some(class(*character)) == start)
                    .find(|(_, character)| class(*character) != 0)
                    .map_or(text.len(), |(index, _)| cursor + index)
            }
            Self::WordLeft => {
                let mut characters = text[..cursor]
                    .char_indices()
                    .rev()
                    .skip_while(|(_, character)| character.is_whitespace())
                    .peekable();
                let Some(word) = characters.peek().map(|(_, character)| class(*character)) else {
                    return 0;
                };
                characters
                    .take_while(|(_, character)| class(*character) == word)
                    .last()
                    .map_or(0, |(index, _)| index)
            }
        }
    }
}

pub fn register_app_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    files::register_commands(registry)?;
    language::register_commands(registry)?;
    debugger::register_commands(registry)?;
    project::register_commands(registry)?;
    packages::register_commands(registry)?;
    windows::register_commands(registry)?;
    terminal::register_commands(registry)?;
    tasks::register_commands(registry)?;
    workspace::register_commands(registry)?;
    configuration::register_commands(registry)?;
    editing::register_commands(registry)?;
    agent::register_commands(registry)?;
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

fn register<F, R>(
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
    invocation: &'a crate::editor::CommandInvocation,
    name: &str,
) -> Result<&'a str, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_str()
}

fn optional_string_arg<'a>(
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

fn optional_bool_arg(invocation: &crate::editor::CommandInvocation, name: &str) -> Option<bool> {
    invocation
        .arguments
        .get(name)
        .and_then(|value| match value {
            CommandValue::Bool(value) => Some(*value),
            _ => None,
        })
}

fn one_character_arg(
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

fn optional_integer_arg(invocation: &crate::editor::CommandInvocation, name: &str) -> Option<i64> {
    match invocation.arguments.get(name) {
        Some(CommandValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn integer_arg(
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

fn bool_arg(
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

#[cfg(test)]
mod tests {
    use super::PromptMove;

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
    }
}
