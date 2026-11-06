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

use crate::editor::ArgumentSchema;

use crate::editor::ArgumentType;

use crate::editor::CommandError;

use crate::editor::CommandOutcome;

use crate::editor::CommandRegistry;

use crate::editor::CommandSpec;

use crate::editor::CommandValue;

use crate::editor::ContextRequirement;

use crate::editor::FocusDirection;
use crate::editor::input::GrammarInput;
use crate::ui::ConfirmationChoice;
use crate::ui::PromptKind;

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
    WindowResizeDirectional(FocusDirection, u64),
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
    ViewToggle(crate::app::ViewToggle),
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
        count: u64,
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

pub use crate::app::windows::LayoutAction;

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
    crate::app::agent::commands::register_commands(registry)?;
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
    use crate::app::commands::PromptMove;

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

use crate::app::persistence::MARK_LIMIT;
use crate::ui::render::SCRATCH_LABEL;
use crate::app::commands::SyntaxFoldAction;
use crate::app::AdapterContext;
use crate::app::AppEffect;
use crate::app::CommandInvocation;
use crate::app::DapControl;
use crate::app::DapSourceBreakpoint;
use crate::app::DiagnosticOwner;
use crate::app::DockEdge;
use crate::app::EditorMode;
use crate::app::App;
use crate::app::LargeFileOverride;
use crate::app::LspFeature;
use crate::app::NativeSurfaceTarget;
use crate::app::PackageOperation;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerPayload;
use crate::app::PickerSurfaceKind;
use crate::app::PromptKind;
use crate::app::RegistryRequest;
use crate::app::TerminalInput;
use crate::app::TrustedAction;
use crate::app::WindowPlacement;
use crate::app::WindowRect;
use crate::app::WorkspaceTrust;
use crate::app::builtin_languages;
use crate::app::mode_name_large;
use crate::app::plugins::VirtualContext;
use slotmap::Key;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::app::PtyId;
use crate::app::CommandValue;
use crate::app::PtyId;
use crate::app::command;
use crate::app::language_for_document;
use crate::windows::WindowPlacement;
use std::ops::ControlFlow::Continue;
use crate::app::PluginStatus;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;


impl App {

    pub(in crate::app) fn process_effect(&mut self, effect: AppEffect) {
        self.process_effect_checked(effect);
    }
}

impl App {


    pub(in crate::app) fn process_effect_checked(&mut self, effect: AppEffect) -> bool {
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
        let effect = match self.process_core_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_configuration_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_terminal_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_extensions_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_project_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_language_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        let effect = match self.process_debugger_effect(effect) {
            Ok(()) => return true,
            Err(effect) => effect,
        };
        if self.process_misc_effect(effect).is_err() {
            unreachable!("every AppEffect variant has one owning handler");
        }
        true
    }
}

impl App {

    pub(in crate::app) fn process_configuration_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::SelectTheme | AppEffect::SetTheme(_) => {
                self.close_picker();
                match self.configuration.begin_theme_picker() {
                    Ok(rows) => self.present_theme_picker(rows),
                    Err(error) => self.show_error(error),
                }
                self.configuration.want_theme(match effect {
                    AppEffect::SetTheme(name) => Some(name),
                    _ => None,
                });
            }
            AppEffect::ReloadConfiguration => match self.configuration.request_reload() {
                Ok(()) => {
                    self.notify_message(self.message("config_reload_queued", &[]), false);
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ExplainConfig(key) => {
                if let Some(explanation) = self.configuration.explain(&key) {
                    self.notify_message(
                        self.message(
                            "config_explanation",
                            &[
                                ("key", explanation.key),
                                ("value", explanation.value),
                                ("source", explanation.source),
                            ],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("unknown_config_key", &[("key", key)]));
                }
            }
            AppEffect::InspectKeymap {
                id,
                context,
                key,
                command,
            } => {
                let context = context.unwrap_or_else(|| self.active_input_context());
                let inspection = id
                    .as_deref()
                    .and_then(|id| self.configuration.keymaps().inspect_id(id))
                    .or_else(|| {
                        key.as_deref().and_then(|keys| {
                            self.configuration.keymaps().inspect_key(&context, keys)
                        })
                    })
                    .or_else(|| {
                        command.as_deref().and_then(|command| {
                            self.configuration.keymaps().inspect(&context, command)
                        })
                    });
                if let Some(inspection) = inspection {
                    self.notify_message(
                        self.message(
                            "keymap_inspection",
                            &[
                                ("id", inspection.id),
                                ("context", inspection.context),
                                ("source_keys", inspection.source_keys),
                                ("keys", inspection.keys),
                                ("operation", inspection.operation.as_str().into()),
                                ("command", inspection.command.unwrap_or_default()),
                                ("arguments", format!("{:?}", inspection.arguments)),
                                ("source", inspection.source),
                            ],
                        ),
                        false,
                    );
                } else if id.is_none() && command.is_none() && key.is_none() {
                    let count = self
                        .configuration
                        .keymaps()
                        .reverse
                        .keys()
                        .filter(|(mapped_context, _)| mapped_context == &context)
                        .count();
                    self.notify_message(
                        self.message(
                            "keymap_context_summary",
                            &[("context", context), ("count", count.to_string())],
                        ),
                        false,
                    );
                } else {
                    self.show_error(self.message("no_keymap_binding", &[("context", context)]));
                }
            }
            AppEffect::ShowKeymapConflicts => {
                let conflicts = self
                    .configuration
                    .rejected_keymaps()
                    .into_iter()
                    .chain(
                        self.configuration
                            .keymaps()
                            .conflicts
                            .iter()
                            .map(String::as_str),
                    )
                    .collect::<Vec<_>>();
                if conflicts.is_empty() {
                    self.notify_message(self.message("no_keymap_conflicts", &[]), false);
                } else {
                    self.show_error(conflicts.join("; "));
                }
            }
            AppEffect::RecoveryInspect => self.open_recovery_picker(),
            AppEffect::SessionSave => {
                if self.configuration.settings().persistence.enabled {
                    self.persistence.request_save_notice();
                    self.write_persistence();
                } else {
                    self.show_error(self.message("session_disabled", &[]));
                }
            }
            AppEffect::SessionLoad => self.load_session_on_request(),
            AppEffect::HistoryClear => {
                self.input.clear_history();
                self.notify_message(self.message("history_cleared", &[]), false);
            }
            AppEffect::LargeFileSet(mode) => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let mode = match mode.as_str() {
                    "auto" => LargeFileOverride::Auto,
                    "on" => LargeFileOverride::On,
                    "off" => LargeFileOverride::Off,
                    _ => {
                        self.show_error(self.message("large_file_invalid", &[]));
                        return Ok(());
                    }
                };
                self.documents.set_large_file_mode(
                    buffer,
                    self.state.buffers[buffer].path.as_deref(),
                    mode,
                );
                self.process_large_file_modes();
                self.notify_message(
                    self.message(
                        "large_file_changed",
                        &[("mode", mode_name_large(mode).into())],
                    ),
                    false,
                );
            }
            AppEffect::LargeFileStatus => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return Ok(());
                };
                let configured = self.documents.configured_mode(buffer);
                let effective = if self.effective_large_file(buffer) {
                    "on"
                } else {
                    "off"
                };
                self.notify_message(
                    self.message(
                        "large_file_status",
                        &[
                            ("mode", mode_name_large(configured).into()),
                            ("effective", effective.into()),
                        ],
                    ),
                    false,
                );
            }
            AppEffect::SyntaxFold(action) => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    let folds = self
                        .ui
                        .syntax_folds
                        .iter_mut()
                        .filter(|fold| fold.view == view);
                    match action {
                        SyntaxFoldAction::OpenAll => folds.for_each(|fold| fold.collapsed = false),
                        SyntaxFoldAction::CloseAll => folds.for_each(|fold| fold.collapsed = true),
                        SyntaxFoldAction::Toggle
                        | SyntaxFoldAction::Open
                        | SyntaxFoldAction::Close => {
                            // The innermost fold around the cursor, the one Vim's `za` acts on.
                            if let Some(fold) = folds
                                .filter(|fold| {
                                    line.is_some_and(|line| {
                                        line >= fold.start_line && line <= fold.end_line
                                    })
                                })
                                .max_by_key(|fold| fold.start_line)
                            {
                                fold.collapsed = match action {
                                    SyntaxFoldAction::Open => false,
                                    SyntaxFoldAction::Close => true,
                                    _ => !fold.collapsed,
                                };
                            } else {
                                self.notify_message(self.message("syntax_no_fold", &[]), false);
                            }
                        }
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| {
                        crate::language::syntax::language_for_document(
                            buffer.path.as_deref(),
                            buffer.text.chunks().next().unwrap_or_default(),
                        )
                    });
                let items = builtin_languages()
                    .enumerate()
                    .map(|(index, info)| {
                        let state = if Some(info.language) == active_language {
                            if let Some(buffer) = active {
                                if let Some(error) = self.syntax.failure(buffer) {
                                    self.message(
                                        "syntax_state_failed",
                                        &[("error", error.to_owned())],
                                    )
                                } else if self.syntax.result(buffer).is_some_and(|result| {
                                    self.state.buffers.get(buffer).is_some_and(|state| {
                                        state.text.revision().0 == result.revision
                                    }) && result.language == info.language
                                }) {
                                    self.message(
                                        if self
                                            .syntax
                                            .result(buffer)
                                            .is_some_and(|result| result.truncated)
                                        {
                                            "syntax_state_active_truncated"
                                        } else {
                                            "syntax_state_active"
                                        },
                                        &[],
                                    )
                                } else {
                                    self.message("syntax_state_pending", &[])
                                }
                            } else {
                                self.message("syntax_state_available", &[])
                            }
                        } else {
                            self.message("syntax_state_available", &[])
                        };
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: info.language.name().to_owned(),
                                detail: self.message(
                                    "syntax_capabilities",
                                    &[
                                        (
                                            "highlights",
                                            self.syntax_support_label(info.capabilities.highlights),
                                        ),
                                        (
                                            "folds",
                                            self.syntax_support_label(info.capabilities.folds),
                                        ),
                                        (
                                            "indents",
                                            self.syntax_support_label(info.capabilities.indents),
                                        ),
                                        ("state", state),
                                    ],
                                ),
                                source: self.message("syntax_builtin_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("syntax_status_title", &[]), items);
            }
            AppEffect::TestRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                // Without a matcher there are no per-test rows, but the run and its exit code stand.
                match task.matcher.clone() {
                    Some(matcher) => self.run_trusted(TrustedAction::RunTest { name, matcher }),
                    None => self.run_trusted(TrustedAction::RunTask(name)),
                }
            }
            AppEffect::TestStop(name) => self.stop_task(&name),
            AppEffect::TestList(name) => self.show_test_picker(&name),
            AppEffect::TaskList => self.show_task_picker(),
            AppEffect::TaskRestart(name) => {
                let adapter = self.tasks.last_adapter(&name).cloned();
                // A finished task has nothing to stop: restarting just runs it again.
                if self.tasks.is_running(&name) {
                    self.stop_task(&name);
                }
                match adapter {
                    Some(AdapterContext::Test { matcher }) => {
                        self.run_trusted(TrustedAction::RunTest { name, matcher })
                    }
                    Some(AdapterContext::Linter {
                        matcher,
                        lint: true,
                    }) => self.run_trusted(TrustedAction::RunLinter { name, matcher }),
                    Some(AdapterContext::Formatter { source }) => {
                        match crate::app::tasks::FormatterSource::capture(
                            &self.state,
                            source.buffer(),
                        ) {
                            Ok(source) => {
                                self.run_trusted(TrustedAction::RunFormatter { name, source })
                            }
                            Err(error) => self.show_error(error),
                        }
                    }
                    _ => self.run_trusted(TrustedAction::RunTask(name)),
                }
            }
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings().tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return Ok(());
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return Ok(());
                };
                self.run_trusted(TrustedAction::RunLinter { name, matcher });
            }
            AppEffect::LintClear(name) => {
                if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                    self.diagnostics.clear_owner(&owner);
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::FormatRun(name) => {
                let Some(view_id) = self.state.active_view else {
                    return Ok(());
                };
                let Some(view) = self.state.views.get(view_id) else {
                    return Ok(());
                };
                let buffer = view.buffer;
                match crate::app::tasks::FormatterSource::capture(&self.state, buffer) {
                    Ok(source) => self.run_trusted(TrustedAction::RunFormatter { name, source }),
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::TaskRun(name) => self.run_trusted(TrustedAction::RunTask(name)),
            AppEffect::TaskStop(name) => self.stop_task(&name),
            AppEffect::TaskOutput(name) => self.show_output_picker(&name),
            AppEffect::CrashInspect => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                self.notify_message(
                    self.message("crash_location", &[("path", path.display().to_string())]),
                    path.is_file(),
                );
            }
            AppEffect::CrashClear => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                if let Err(error) = self
                    .recovery
                    .clear_crash(path, self.documents.recovery_io())
                {
                    self.show_error(error);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {

    pub(in crate::app) fn process_debugger_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        self.debugger_session.refresh(
            &self.configuration.settings().debug,
            self.workspace.version(),
        );
        match effect {
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
                let Some(view) = self.state.active_view else {
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

impl App {

    pub(in crate::app) fn process_extensions_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::PluginValidate => self.load_configured_plugins(),
            AppEffect::PluginList => {
                let items: Vec<_> = self
                    .extension
                    .statuses()
                    .iter()
                    .take(4096)
                    .enumerate()
                    .map(|(index, (id, status))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("plugin:{index}:{id}")),
                                label: id.clone(),
                                detail: format!("{status:?}"),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                let empty = items.is_empty();
                self.install_picker(self.message("plugin_picker_title", &[]), items);
                if empty {
                    self.search
                        .set_picker_footer(self.message("plugin_picker_empty", &[]));
                    self.sync_picker_ui();
                }
            }
            AppEffect::PluginDisable(id) => {
                if !self.extension.statuses().contains_key(&id) {
                    self.show_error(self.message("plugin_unknown", &[("id", id)]));
                    return Ok(());
                }
                let load = self.extension.load_id(&id);
                if let Err(error) = self.extension.disable(&id) {
                    self.show_error(error)
                } else {
                    if let Some(load) = load {
                        self.remove_plugin_contribution_generation(&id, load);
                    }
                    self.commands.unregister_plugin(&id);
                    self.notify_message(self.message("plugin_disabled", &[("id", id)]), false);
                }
            }
            AppEffect::PluginPickerOpen { provider, query } => {
                self.open_plugin_picker(provider, query)
            }
            AppEffect::VirtualDocumentOpen { source, key } => {
                let result = self.extension.open_virtual(&source, key, &self.state);
                self.route_virtual_open(result);
            }
            kind @ (AppEffect::HealthVirtualOpen | AppEffect::HealthVirtualRefresh) => {
                let result = self
                    .extension
                    .prepare_health(
                        matches!(kind, AppEffect::HealthVirtualRefresh),
                        self.commands.specs().count(),
                        VirtualContext {
                            editor: &self.state,
                            windows: &self.window_session,
                            scratch: SCRATCH_LABEL,
                        },
                    )
                    .and_then(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    });
                match result {
                    Ok(state) => {
                        *self.state = state;
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::VirtualDocumentRefresh => {
                let result = self
                    .state
                    .active_buffer_id()
                    .ok_or_else(|| "no active virtual document".to_owned())
                    .and_then(|buffer| self.extension.refresh_virtual(buffer, &self.state));
                self.route_virtual_open(result);
            }
            AppEffect::PluginExecute {
                plugin,
                command,
                arguments,
            } => {
                self.execute_plugin_command(plugin, command, arguments);
            }

            AppEffect::PackageInstall {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                } else {
                    self.run_trusted(TrustedAction::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    });
                }
            }
            AppEffect::PackageRegistrySearch(query) => {
                self.request_registry(RegistryRequest::Search { query }, false)
            }
            AppEffect::PackageRegistryShow {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                false,
            ),
            AppEffect::PackageRegistryInstall {
                publisher,
                name,
                version,
            } => {
                if self.workspace.safe_mode() {
                    self.show_error(self.message("safe_mode_blocked", &[]))
                } else {
                    self.run_trusted(TrustedAction::InstallRegistry {
                        publisher,
                        name,
                        version,
                    })
                }
            }
            AppEffect::PackageList => {
                if let Err(error) = self.extension.request_package(PackageOperation::List) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageVerify => {
                if let Err(error) = self.extension.request_package(PackageOperation::Verify) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRollback => {
                if let Err(error) = self.extension.request_package(PackageOperation::Rollback) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRemove(name) => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::Remove(name))
                {
                    self.show_error(error)
                }
            }
            AppEffect::PackageGarbageCollect => {
                if let Err(error) = self
                    .extension
                    .request_package(PackageOperation::GarbageCollect { maximum: 256 })
                {
                    self.show_error(error)
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {


    pub(in crate::app) fn process_language_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Settings(action) => self.settings_action(action),
            AppEffect::Setup(action) => self.setup_action(action),
            AppEffect::OpenToolMenu(category) => self.open_tool_menu(category),
            // Esc on a filtered list clears the search before it closes the manager.
            AppEffect::CloseToolMenu
                if self
                    .ui
                    .tool_menu
                    .as_ref()
                    .is_some_and(|menu| !menu.query.is_empty()) =>
            {
                self.search_tool_menu(None, false);
            }
            AppEffect::CloseToolMenu => self.close_tool_menu(),
            AppEffect::ToolSearch => self.search_tool_menu(None, true),
            AppEffect::ToolSearchInsert(text) => {
                let query = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map(|menu| format!("{}{text}", menu.query));
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchDelete => {
                let query = self.ui.tool_menu.as_ref().map(|menu| {
                    let mut query = menu.query.clone();
                    query.pop();
                    query
                });
                self.search_tool_menu(query, true);
            }
            AppEffect::ToolSearchStop { keep } => {
                let query = self.ui.tool_menu.as_ref().map(|menu| menu.query.clone());
                self.search_tool_menu(query.filter(|_| keep), false);
            }
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
            AppEffect::LspDocumentationScroll(direction) => {
                self.reconcile_editor_windows();
                // Use the same final layout as painting, including a narrowed completion box.
                let (_, geometry) = crate::ui::render::render_with_documentation_geometry(
                    &self.state,
                    &self.ui,
                    self.configuration.settings(),
                    self.configuration.dashboard(),
                    self.window_session.width(),
                    self.window_session.height(),
                );
                if let Some((visible, total)) = geometry {
                    self.input.scroll_documentation(
                        direction,
                        crate::ui::render::documentation::DOCUMENTATION_SCROLL_STEP,
                        visible,
                        total,
                    );
                }
                self.project_input_documentation();
            }
            AppEffect::LspDocumentationClose => self.close_documentation(),
            AppEffect::LspCompletionDocumentationOpen => {
                self.open_lsp_completion_documentation(true)
            }
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::DiagnosticNavigate {
                direction,
                filter,
                count,
            } => {
                self.navigate_diagnostic(direction, filter, count);
            }
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspRenamePrompt => self.open_rename_prompt(),
            AppEffect::Substitute {
                range,
                pattern,
                replacement,
                flags,
            } => self.run_substitute(&range, &pattern, &replacement, &flags),
            AppEffect::GlobalDelete {
                range,
                pattern,
                invert,
            } => self.run_global_delete(&range, &pattern, invert),
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
                if let Some(view) = self.state.active_view {
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

impl App {

    pub(in crate::app) fn process_misc_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::MarkJump(name) => {
                self.persistence.reconcile_marks(&self.state);
                let location = self
                    .persistence
                    .marks()
                    .iter()
                    .find(|mark| mark.name == name)
                    .map(|mark| mark.location.clone());
                match location {
                    Some(location) => self.navigate_persisted_location(location),
                    None => self.show_error(self.message("mark_unset", &[("name", name)])),
                }
            }
            AppEffect::MarkSet(name) => {
                if let Some(location) = self.current_persisted_location() {
                    self.persistence
                        .set_mark(name, location, MARK_LIMIT, &self.state);
                    self.persistence.reconcile_marks(&self.state);
                }
            }

            AppEffect::SafeModeStatus => {
                self.notify_message(
                    self.message(
                        "safe_mode_status",
                        &[(
                            "state",
                            if self.workspace.safe_mode() {
                                "active".into()
                            } else {
                                "inactive".into()
                            },
                        )],
                    ),
                    false,
                );
            }
            AppEffect::ShowHealth => {
                self.notify_message(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.notices.terminal_capabilities().to_owned()),
                            ("jobs", self.tasks.running_count().to_string()),
                            ("ptys", self.terminal_session.running_count().to_string()),
                            ("preflights", self.pending_preflight_count().to_string()),
                            ("tasks", self.tasks.status_count().to_string()),
                            ("plugins", self.extension.statuses().len().to_string()),
                            (
                                "plugin_failures",
                                self.extension
                                    .statuses()
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::plugins::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.extension.pending_packages().to_string()),
                            ("lsp", self.language.sessions().len().to_string()),
                            ("persistence", self.persistence.pending().to_string()),
                            ("safe", self.workspace.safe_mode().to_string()),
                            ("dap", self.debugger_session.is_running().to_string()),
                        ],
                    ),
                    false,
                );
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {

    pub(in crate::app) fn process_project_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceTrust => self.request_workspace_trust(),
            AppEffect::WorkspaceUntrust => self.revoke_workspace_trust(),
            AppEffect::WorkspaceStatus => {
                let state = if self.workspace.trust() == WorkspaceTrust::Trusted {
                    self.message("workspace_trust_trusted", &[])
                } else {
                    self.message("workspace_trust_untrusted", &[])
                };
                self.notify_message(
                    self.message("workspace_trust_status", &[("state", state)]),
                    false,
                );
            }
            AppEffect::ExplorerPanelToggle => {
                return self.process_project_effect(if self.ui.explorer.is_some() {
                    AppEffect::ExplorerClose
                } else {
                    AppEffect::ExplorerOpen
                });
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerSearch => {
                self.explorer.start_search();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerSearchInsert(text) => {
                let query = format!("{}{text}", self.explorer.tree().filter());
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchDelete => {
                let mut query = self.explorer.tree().filter().to_owned();
                query.pop();
                self.search_explorer(query);
            }
            AppEffect::ExplorerSearchStop { keep } => {
                self.explorer.stop_search(keep);
                self.sync_explorer_ui();
            }
            // Esc on a filtered tree clears the filter before it closes the explorer.
            AppEffect::ExplorerClose if !self.explorer.tree().filter().is_empty() => {
                self.explorer.stop_search(false);
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerClose => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
                self.ensure_cursor_visible();
            }
            AppEffect::ExplorerMove(delta) => {
                self.move_explorer_selection(isize::from(delta));
            }
            AppEffect::ExplorerPage(direction) => self.page_explorer_selection(direction),
            AppEffect::ExplorerParent => self.explorer_collapse_or_parent(),
            AppEffect::ExplorerChild => {
                self.explorer.select_first_child();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerToggle => self.explorer_toggle(),
            AppEffect::ExplorerRefresh => {
                self.refresh_explorer();
                self.start_git_status_refresh_with_feedback(false);
            }
            AppEffect::ExplorerFollow => self.follow_active_file_in_explorer(),
            AppEffect::ExplorerToggleHidden => {
                self.explorer.toggle_hidden();
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer.toggle_ignored();
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
            AppEffect::ExplorerCreate(path) => self.create_explorer_path(path),
            AppEffect::ExplorerRename(target) => self.rename_explorer_path(target),
            AppEffect::ExplorerRenamePrompt => self.open_explorer_rename_prompt(),
            AppEffect::ExplorerDeleteRequest => self.request_explorer_delete(),
            AppEffect::ExplorerClipboard { cut } => self.set_explorer_clipboard(cut),
            AppEffect::ExplorerPaste => self.paste_explorer_clipboard(),
            AppEffect::ExplorerYankPath => self.yank_explorer_path(),
            AppEffect::ExplorerMouseSelect => self.explorer_mouse_select(),
            AppEffect::WorkspaceRootAdd(path) => self.add_workspace_root(path),
            AppEffect::WorkspaceRootActivate(id) => {
                self.activate_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootRemove(id) => {
                self.remove_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootStatus => {
                if let Some(root) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.notify_message(
                        self.message(
                            "workspace_root_status",
                            &[
                                ("id", root.id.0.to_string()),
                                ("path", root.identity.canonical_path.display().to_string()),
                            ],
                        ),
                        false,
                    );
                }
            }
            AppEffect::WorkspaceSearchPrompt => self.open_live_workspace_search(),
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.search.cancel_grep();
                self.notify_message(self.message("workspace_search_cancelled", &[]), false);
            }
            AppEffect::WorkspaceReplace { query, replacement } => {
                self.start_workspace_search(query, Some(replacement));
            }
            AppEffect::OpenPicker(kind) => self.open_builtin_picker(kind),
            AppEffect::PickerInsert(text) => self.picker_insert(&text),
            AppEffect::PickerBackspace => self.picker_backspace(),
            AppEffect::PickerMove(direction) => self.picker_move(direction),
            AppEffect::PickerPreviewPage(direction) => {
                self.search.picker_preview_page(direction);
                self.sync_picker_ui();
            }
            AppEffect::PickerResults => {
                if self.search.set_picker_surface(PickerSurfaceKind::Problems) {
                    self.sync_picker_ui();
                }
            }
            AppEffect::PickerRemove => match self.search.selected_picker_payload().cloned() {
                Some(PickerPayload::Recovery(journal)) => {
                    if let Err(error) =
                        self.recovery
                            .discard(&journal, true, self.documents.recovery_io())
                    {
                        self.show_error(error);
                    }
                }
                Some(PickerPayload::GitWorktree { .. }) => self.request_git_worktree_remove(),
                _ => {}
            },
            AppEffect::PickerAccept => self.accept_picker(),
            AppEffect::PickerCancel => self.close_picker(),
            AppEffect::PickerNormal if self.palette_is_open() => self.close_picker(),
            AppEffect::PickerNormal | AppEffect::PickerInsertMode => {
                if self.search.picker().is_some() && self.search.workspace_picker().is_none() {
                    self.input
                        .set_picker_normal(matches!(effect, AppEffect::PickerNormal));
                    self.sync_picker_ui();
                }
            }
            AppEffect::WorkspaceEditToggle => {
                self.retire_stale_picker();
                let selected = self
                    .selected_picker_payload()
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected
                    && self.workspace_edits.toggle(index).is_ok()
                {
                    self.show_workspace_proposal();
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => loop {
                let proposal = self.workspace_edits.next_resource_proposal();
                let result = match self.workspace_edits.prepare_resource(
                    &self.workspace,
                    self.configuration.binding_revision_ref(),
                    &self.state,
                ) {
                    Ok(Some(permit)) => self.documents.submit_workspace_resource(permit),
                    Ok(None) => break,
                    Err(error) => {
                        self.language.cancel_deferred_edits(proposal);
                        self.show_error(error);
                        break;
                    }
                };
                match result {
                    Ok(true) => {}
                    Ok(false) => {
                        self.notify_message(
                            "Document queue is full; remaining resources are still staged. Apply them again after pending work finishes.".into(),
                            false,
                        );
                        break;
                    }
                    Err(error) => {
                        self.language.cancel_deferred_edits(proposal);
                        self.show_error(error);
                        break;
                    }
                }
            },
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {


    pub(in crate::app) fn process_terminal_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::TerminalOpen(profile) => {
                let profile = profile.unwrap_or_else(|| {
                    self.configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone()
                });
                self.request_terminal_open(&profile, None);
            }
            AppEffect::TerminalToggle => {
                // This is the panel's terminal. A side terminal, such as a docked agent, keeps its
                // window, so from one this opens the panel beside it.
                let focused = self
                    .window_session
                    .focused_terminal()
                    .map(PtyId)
                    .filter(|id| self.terminal_session.contains(*id))
                    .filter(|id| !self.terminal_session.side(*id));
                if let Some(id) = focused {
                    if let Err(error) = self.apply_native_window_placement(
                        NativeSurfaceTarget::Terminal(id.0),
                        WindowPlacement::Hidden,
                    ) {
                        self.show_error(error);
                    }
                } else if let Some(id) = self.terminal_session.panel() {
                    self.reveal_terminal(id);
                } else {
                    let profile = self
                        .configuration
                        .settings()
                        .terminals
                        .default_profile
                        .clone();
                    self.request_terminal_open(&profile, None);
                }
            }
            AppEffect::TerminalList => {
                let items = self
                    .terminal_session
                    .choices()
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, label, detail))| {
                        (
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("terminal:{index}")),
                                label,
                                detail,
                                source: "terminal".into(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Terminal(key),
                        )
                    })
                    .collect();
                self.install_picker("Terminals".into(), items);
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminal_session.select(id) {
                    self.reveal_terminal(id);
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalFocusActive => {
                if self.terminal_session.selected().is_some() {
                    self.set_terminal_focus(true);
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalCycle(direction) => {
                let focused = self.window_session.terminal_focused();
                let hidden = self.terminal_window_rect().is_none();
                if !self.terminal_session.cycle(i64::from(direction)) {
                    self.show_error(self.message("terminal_missing", &[]));
                } else if let Some(id) = self
                    .terminal_session
                    .selected()
                    .filter(|_| focused || hidden)
                {
                    self.reveal_terminal(id);
                } else {
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                }
            }
            AppEffect::TerminalResize(rows) => {
                if let Some(id) = self.displayed_terminal() {
                    let result = if self.compositor_projection().is_none() {
                        Err("universal compositor projection is missing or stale".into())
                    } else {
                        self.window_session.resize_terminal(id.0, rows)
                    };
                    if let Err(error) = result {
                        self.show_error(error);
                    }
                    self.resize_active_terminal_to_panel();
                    self.sync_terminal_panel();
                    self.sync_explorer_ui();
                    self.ensure_cursor_visible();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalDock { edge, size } => {
                let edge = match edge.as_str() {
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    "top" => DockEdge::Top,
                    "bottom" => DockEdge::Bottom,
                    _ => {
                        self.show_error("terminal dock edge must be left, right, top, or bottom");
                        return Ok(());
                    }
                };
                self.place_active_terminal(WindowPlacement::Docked { edge, size });
            }
            AppEffect::TerminalFloat(rect) => {
                let rect = rect.unwrap_or_else(|| {
                    let (columns, rows) =
                        (self.window_session.width(), self.window_session.height());
                    let (width, height) = (
                        (u32::from(columns) * 4 / 5) as u16,
                        (u32::from(rows) * 4 / 5) as u16,
                    );
                    WindowRect {
                        x: (columns - width) / 2,
                        y: (rows - height) / 2,
                        width,
                        height,
                    }
                });
                self.place_active_terminal(WindowPlacement::Floating(rect));
            }
            AppEffect::TerminalFullscreen => {
                self.place_active_terminal(WindowPlacement::Fullscreen)
            }
            AppEffect::TerminalHide => self.place_active_terminal(WindowPlacement::Hidden),
            AppEffect::TerminalUnfocus => {
                self.set_terminal_focus(false);
                self.sync_terminal_panel();
                self.ensure_cursor_visible();
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.terminal_session.selected()
                    && let Err(error) = self.terminal_session.send(
                        id,
                        match text.as_str() {
                            "\x1b[A" => TerminalInput::Key(crate::ui::PtyInputKey::Up),
                            "\x1b[B" => TerminalInput::Key(crate::ui::PtyInputKey::Down),
                            "\x1b[C" => TerminalInput::Key(crate::ui::PtyInputKey::Right),
                            "\x1b[D" => TerminalInput::Key(crate::ui::PtyInputKey::Left),
                            "\x1b[H" => TerminalInput::Key(crate::ui::PtyInputKey::Home),
                            "\x1b[F" => TerminalInput::Key(crate::ui::PtyInputKey::End),
                            "\x1b[5~" => TerminalInput::Key(crate::ui::PtyInputKey::PageUp),
                            "\x1b[6~" => TerminalInput::Key(crate::ui::PtyInputKey::PageDown),
                            _ => TerminalInput::Text(&text),
                        },
                        &self.workspace,
                    )
                {
                    self.show_error(error);
                }
            }
            AppEffect::TerminalModeNormal => self.enter_terminal_normal_mode(),
            AppEffect::TerminalModeInput => self.enter_terminal_input_mode(),
            AppEffect::TerminalNormalMove(motion) => {
                self.move_terminal_normal_cursor(&motion);
            }
            AppEffect::TerminalVisualStart(linewise) => {
                self.start_terminal_visual(linewise);
            }
            AppEffect::TerminalYank { line } => self.yank_terminal_text(line),
            AppEffect::TerminalClose => {
                if let Some(id) = self.displayed_terminal() {
                    self.close_terminal(id);
                } else {
                    self.terminal_session.cancel_open();
                }
            }
            AppEffect::TerminalQuit => {
                let Some(id) = self.displayed_terminal() else {
                    return Ok(());
                };
                if self.terminal_session.stopping(id) {
                    return Ok(());
                }
                if let Err(error) = self
                    .window_session
                    .require_native_stop(NativeSurfaceTarget::Terminal(id.0))
                {
                    self.show_error(error);
                    return Ok(());
                }
                self.terminal_session.cancel_open();
                // The neighbouring tab takes over while this one stops.
                self.terminal_session.select(id);
                self.terminal_session.cycle(-1);
                match self.terminal_session.selected().filter(|next| *next != id) {
                    Some(next) => self.reveal_terminal(next),
                    None => self.set_terminal_focus(false),
                }
                self.retire_terminal(id, true);
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

impl App {


    pub(in crate::app) fn invoke(&mut self, invocation: CommandInvocation) {
        self.invoke_checked(invocation);
    }
}

impl App {

    pub(in crate::app) fn invoke_checked(&mut self, mut invocation: CommandInvocation) -> bool {
        if self.clipboard.waiting() {
            self.clipboard
                .defer(crate::app::editing::clipboard::DeferredInput::Invocation(
                    invocation,
                ));
            return true;
        }
        if !invocation.id.as_str().starts_with("lsp.documentation.") {
            self.close_documentation();
        }
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view;
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
        }
        if invocation.id.as_str() == "edit.put"
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
        match self.commands.execute(&self.state, &invocation) {
            Ok(outcome) => {
                let text_before = invocation.target.buffer.and_then(|buffer| {
                    self.state
                        .buffers
                        .get(buffer)
                        .map(|buffer| buffer.text.source_version())
                });
                if let Some(mut transaction) = outcome.transaction {
                    self.persistence.reconcile_marks(&self.state);
                    let source = self.state.window_version();
                    let snippet_document = self.input.snippet_buffer().and_then(|id| {
                        self.state.buffers.get(id).map(|buffer| {
                            crate::app::input::SnippetDocument {
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
                                crate::app::input::SnippetDocument {
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
                self.input.record_successful_invocation(
                    &invocation,
                    repeat_override.as_ref(),
                    suppress_repeat,
                    text_changed,
                );
                self.tutor_observe(&invocation);
                for effect in outcome.effects {
                    if !self.process_effect_checked(effect) {
                        return false;
                    }
                }
                self.sync_flash_ui();
                true
            }
            Err(error) => {
                // A failed `dfz` or `di(` must not leave the view stuck in operator-pending.
                if self
                    .state
                    .active_view
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
