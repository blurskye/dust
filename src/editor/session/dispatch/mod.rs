use super::configuration::ConfigurationSession;
#[cfg(test)]
use super::configuration::{
    KeymapOperation, compile_keymaps, compile_keymaps_with_plugin_catalog,
    configuration_generation_is_newer, reload_keymaps, resolve_configuration,
    validate_configured_commands,
};
use super::documents::{DocumentEvent, DocumentSession, LargeFileOverride, mode_name_large};
use super::input::InputSession;
use super::lsp::{LspAutostartObservation, LspLaunch};
use super::lsp::{LspLaunchRequest, LspLaunchSource, PendingLspProjectRoot};
#[cfg(test)]
use super::lsp::{LspProjectionOwner, LspRuntimeState};
use super::persistence::{RestoreContext, RestorePrepared, RestoreReady, RestoreUpdate};
use super::search::{MatcherSource, PickerContent, PickerOrigin, PickerPayload};
use super::terminal::{TerminalInput, TerminalUpdate};
use super::tooling::{
    LspRuntimeSource, ToolInstall, lsp_runtime_source_allowed_on_host,
    nix_workspace_shell_for_host, tool_kind_key,
};
use super::windows::{
    EditorCloseSave, EditorCloseStep, EditorCloseTarget, EditorWindowBridge, LayoutPlanError,
    NativeDescriptions, NativeSurfaceSignature, NativeSurfaceTarget, ProjectionRequest,
    RestoreWindowPolicy, WindowCandidate, WindowSession,
};
#[path = "../commands/mod.rs"]
pub(super) mod commands;
use crate::editor::session::debugger::DebugSession;
pub(super) mod agent;
mod conflict_markers;
mod effects;
mod events;
mod palette;
mod pull_requests;
mod remote;
mod settings;
mod substitute;

#[cfg(test)]
use super::persistence::capture_persisted_layout;
use super::syntax::{IndentError, SyntaxDocument, SyntaxSession};
use super::windows::rect_contains_point;
use crate::editor::session::tasks::{AdapterContext, RunningTask};
#[cfg(test)]
use crate::editor::transaction::build_restored_layout;

pub use commands::{
    AppEffect, DiagnosticFilter, LayoutAction, PickerKind, PromptMove, register_app_commands,
};

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::TrustedAction;
use super::workspace::WorkspacePreflightEvent as ExecutionPreflightEvent;
use crate::config::LoadedConfig;
use crate::debugger::{DapControl, DapEvent, DapResponse, DapSourceBreakpoint};
use crate::editor::input::{InputAction, KeyEvent, MappingResolution, parse_ex};
use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, DisplayWidthPolicy, PositionCodec, PositionEncoding,
    TextBuffer, TextEdit, TextEditSet,
};
use crate::editor::{
    ArgumentSchema, CommandError, CommandId, CommandInvocation, CommandOutcome, CommandRegistry,
    CommandSource, CommandSpec, CommandValue, ContextRequirement, EditorMode, EditorOperation,
    EditorState, EditorTransaction, FocusDirection, InvocationSource, Precondition, RegisterShape,
    RegisterValue, TransactionId, UndoPolicy, ViewId, register_builtin_commands,
};
use crate::extensions::{PluginEvent, PluginLimits, PluginLoadId, PluginStatus};
use crate::files::{
    ExplorerMutation, ExplorerWatchService, ResourceIntentKind, VfsError, VfsResult, VfsWorker,
    WorkspaceFilePolicy, WorkspaceSearchMatch, WorkspaceSearchPolicy,
};
use crate::language::lsp::{
    LspDocument, LspEvent, LspFeature, LspFeatureResult, LspMarkupContent, LspMarkupKind,
    LspProgressKind, LspWorkspaceEdit, file_uri, language_id_for_path, path_from_file_uri,
};
use crate::language::syntax::{SyntaxSupport, builtin_languages, language_for_document};
use crate::persistence::{
    PersistedLargeFileOverride, PersistedLocation, PersistedSession, PersistedState,
    PersistedWindowRule, PersistencePolicy, PersistenceService, STATE_VERSION,
};
use crate::process::PtyId;
use crate::telemetry::{LiveCrashSnapshot, LogLevel};
use crate::tools::{
    PackageOperation, RegistryRequest, RegistryResult, RegistryService, ToolDefinition, ToolKind,
    ToolOperation, ToolStatus, discover_executable, is_nixos_host, managed_formatter,
    managed_tool_executable, nix_lsp_launch_environment, nix_lsp_profile_installed,
};
use crate::ui::{
    DocumentationMarkupKind, DocumentationPopupKind, EditorWindowBinding as UiEditorWindowBinding,
    EditorWindowLayoutState, KeyInputKind, NativeSurfaceWindowBinding, PickerSurfaceKind,
    PromptKind, TerminalError, TerminalOptions, TerminalPanelMode, TerminalSession, ToolMenuRow,
    ToolMenuState, ToolRuntimeOption, ToolRuntimePickerState, UiEvent, UiFold, UiInlayHint,
    UiState, UiSyntaxSpan, VisualRowAnchor, VisualRowModel, active_editor_pane_viewport,
    editor_pane_viewport_at, editor_state_content_width, editor_state_gutter_width,
    explorer_panel_width, render as render_frame, screen_foot_rows, snap_display_column,
    snap_display_column_forward, terminal_panel_geometry, terminal_tab_at_column,
    workspace_top_rows,
};
use crate::workspace::{
    DiagnosticOwner, ExecutionSource, ExplorerKind, FileSnapshot, PickerAction, PickerItem,
    PickerItemId, PickerState, RootIdentity, WorkspaceEditDraft, WorkspaceId, WorkspaceState,
    WorkspaceTrust,
};
use thiserror::Error;

use super::extensions::{PluginUiInteraction, WindowUpdate};
use crate::editor::{ApplicationError, StartupTarget, build_state};
use crate::ui::window::{
    CompositorProjection, DockEdge, SpatialDirection, WindowPlacement, WindowRect,
};

pub(crate) const EVENT_POLL_MS: u64 = 16;
pub(crate) const MACRO_RECURSION_LIMIT: usize = 16;
pub(crate) const MACRO_STEP_LIMIT: usize = 10_000;
pub(crate) const REPEAT_STEP_LIMIT: usize = 10_000;
pub(crate) const HALF_PAGE_PERCENT: u8 = 50;
pub(crate) const FULL_PAGE_PERCENT: u8 = 100;

/// A value that counts how often it was borrowed mutably. The main loop rebuilds the frame
/// only when the editor or UI state was touched (or input arrived), so an idle editor does
/// no rendering work at all.
struct Tracked<T> {
    value: T,
    revision: u64,
}

impl<T> Tracked<T> {
    const fn new(value: T) -> Self {
        Self { value, revision: 0 }
    }

    /// Store `value` in the field `slot` picks, counting a change only when it differs. Steps
    /// that run on every main loop pass write through this so an idle pass changes nothing.
    fn set<V: PartialEq>(&mut self, value: V, slot: impl FnOnce(&mut T) -> &mut V) {
        let slot = slot(&mut self.value);
        if *slot != value {
            *slot = value;
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

impl<T> std::ops::Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for Tracked<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.revision = self.revision.wrapping_add(1);
        &mut self.value
    }
}

/// A save waiting for the language server to format the buffer.
struct FormatOnSave {
    request: Option<(String, u64)>,
    buffer: crate::editor::BufferId,
    source: crate::editor::text::TextVersion,
    path: Option<std::path::PathBuf>,
    formatter: Option<String>,
    started: bool,
    quit_after: bool,
    deadline: std::time::Instant,
}

struct InteractiveApplication {
    format_on_save: std::collections::HashMap<crate::editor::BufferId, FormatOnSave>,
    pending_saves: std::collections::VecDeque<(crate::editor::BufferId, bool)>,
    navigation_reads: std::collections::HashMap<
        crate::files::RequestId,
        (super::windows::NavigationStamp, std::sync::Arc<()>),
    >,
    /// The last request someone asked for, until their next key.
    lsp_asked: Option<crate::language::lsp::LspFeature>,
    lsp_asked_generation: Option<u64>,
    /// Asked for before its server could answer, and sent again once it has started and
    /// finished loading.
    lsp_waiting: Option<crate::language::lsp::LspFeature>,
    lsp_waiting_server: Option<String>,
    /// When the waiting request is sent again after a "content modified" answer that came
    /// before the server reported any loading, and how many times it has been.
    lsp_retry_at: Option<std::time::Instant>,
    lsp_retries: u8,
    /// Last seen modification time and length of each open file.
    disk_stamps:
        std::collections::HashMap<crate::editor::BufferId, (Option<std::time::SystemTime>, u64)>,
    reload_stamps:
        std::collections::HashMap<crate::editor::BufferId, (Option<std::time::SystemTime>, u64)>,
    pub(super) next_disk_check: Option<std::time::Instant>,
    /// When the repository's index and HEAD last changed, as of the last disk check.
    pub(super) git_stamp: Option<(
        [Option<std::time::SystemTime>; 2],
        Option<crate::workspace::git::GitHeadId>,
    )>,
    /// What project replace is looking for, while it asks what to replace it with.
    replace_query: Option<String>,
    /// A `:s///c` asking about each match in turn.
    substitute_confirm: Option<substitute::SubstituteConfirm>,
    session_load_requested: bool,
    session_restore_notice: bool,
    notices: super::notifications::NoticeSession,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    state: Tracked<EditorState>,
    extension: super::extensions::ExtensionSession,
    workspace: super::workspace::WorkspaceSession,
    diagnostics: super::diagnostics::DiagnosticSession,
    workspace_edits: super::workspace_edits::WorkspaceEditSession,
    commands: CommandRegistry<AppEffect>,
    input: InputSession,
    configuration: ConfigurationSession,
    ui: Tracked<UiState>,
    documents: DocumentSession,
    search: super::search::SearchSession,
    should_quit: bool,
    /// When the last key arrived, so automatic prompts wait until typing pauses.
    last_key_at: Option<Instant>,
    /// The `s` jump while its labels are shown.
    flash: Option<super::flash::FlashState>,
    /// The ACP agent session and its transcript panel.
    agent: agent::AgentPanel,
    pull_requests: pull_requests::PullRequests,
    tutor: tutor::Tutor,
    /// Commands last run from the palette, newest first.
    palette_recent: Vec<String>,
    /// The split border being dragged with the mouse: the panes on its two sides.
    border_drag: Option<(crate::editor::PaneId, crate::editor::PaneId, bool)>,
    /// The docked terminal whose inner edge is being dragged with the mouse.
    dock_drag: Option<PtyId>,
    /// The column a buffer tab was last dragged at with the mouse.
    tab_drag: Option<u16>,
    /// Where `dust --remote` from Dust's own terminals reaches this session.
    remote: Option<crate::remote::RemoteServer>,
    /// A folder the user was warned that switching to ends the open terminals; asking again
    /// switches.
    switch_ends_terminals: Option<PathBuf>,
    switch_approved_terminals: Vec<PtyId>,
    /// The parse results and views `ui.syntax_spans` and `ui.syntax_folds` were built from.
    #[allow(clippy::type_complexity)]
    syntax_spans_source: (
        Vec<(crate::editor::BufferId, u64, u64)>,
        Vec<(crate::editor::ViewId, crate::editor::BufferId)>,
    ),
    syntax_fold_positions: Vec<
        Option<(
            crate::editor::BufferId,
            crate::editor::text::TextVersion,
            BufferRevision,
            ByteRange,
        )>,
    >,
    window_session: WindowSession,
    tooling: super::tooling::ToolingSession,
    language: super::lsp::LspSession,

    debugger_session: DebugSession,
    debug_terminal: Option<PtyId>,
    recovery: super::recovery::RecoverySession,
    next_transaction: u64,
    git: super::git::GitSession,
    clipboard: super::clipboard::ClipboardSession,
    macro_replays: Vec<(Vec<CommandInvocation>, usize, u64)>,
    macro_running: bool,
    explorer: super::explorer::ExplorerSession,
    syntax: SyntaxSession,
    persistence: super::persistence::PersistenceSession,
    tasks: crate::editor::session::tasks::TaskSession,
    terminal_session: super::terminal::TerminalSession,
}

pub fn run_interactive(
    startup: StartupTarget,
    loaded_config: LoadedConfig,
    safe_mode: bool,
) -> Result<(), InteractiveError> {
    let new_files = startup.new_files();
    let mut application = InteractiveApplication::new(
        startup,
        loaded_config,
        safe_mode,
        crate::config::user_state_directory(),
        true,
        true,
        std::sync::Arc::new(crate::clock::SystemClock),
    )?;
    application
        .window_session
        .configure_initial(&application.configuration.settings().windows)
        .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let crash_path = crate::config::user_state_directory().join("last-crash.json");
    if application.workspace.safe_mode() {
        application.notify_message(
            application.message("safe_mode_status", &[("state", "active".into())]),
            true,
        );
    } else if crash_path.is_file() {
        application.notify_message(application.message("previous_crash", &[]), true);
    }
    application.rebuild_matcher_cache();
    application.start_remote_server();
    for path in new_files {
        application.edit_path(path);
    }
    if !application.workspace.safe_mode() {
        application.load_configured_plugins();
        application.extension.start_packages();
    }
    application.notices.record(
        LogLevel::Info,
        "application",
        "startup",
        BTreeMap::from([
            (
                "safe_mode".into(),
                application.workspace.safe_mode().to_string(),
            ),
            (
                "roots".into(),
                application.workspace.roots().len().to_string(),
            ),
            (
                "buffers".into(),
                application.state.buffers.len().to_string(),
            ),
        ]),
    );
    if !application.workspace.safe_mode() {
        application.load_persistence();
    }
    if application.documents.has_startup_work() {
        application.notify_message(application.message("opening_file", &[]), false);
    }
    let terminal_ui = &application.configuration.settings().ui;
    let mut terminal = TerminalSession::enter(
        TerminalOptions {
            color_mode: terminal_ui.color_mode,
            alternate_screen: true,
            bracketed_paste: true,
            focus_events: true,
            mouse: terminal_ui.mouse,
        },
        application.configuration.theme().clone(),
    )?;
    application
        .notices
        .set_terminal_capabilities(terminal.capability_summary());
    application.window_session.observe_size(terminal.size()?);
    application.clipboard.enable(&application.state.registers);

    // Polls since the screen last changed or input arrived. Background results are only
    // picked up between polls, so the loop polls quickly while things happen and slows to
    // IDLE_POLL once the screen has been still for IDLE_AFTER polls. Input always wakes it.
    const IDLE_AFTER: u32 = 60;
    const IDLE_POLL: Duration = Duration::from_millis(250);
    let mut still_polls = 0u32;
    let mut drawn = None;
    let mut input_arrived = true;
    while !application.should_quit {
        application.process_vfs_results();
        // Git commands need status from the start, not only once a file or the tree is open.
        application.start_git_status_refresh_silent();
        application.process_git_status_events();
        application.process_git_hunk_events();
        application.process_git_sign_events();
        application.process_git_mutation_events();
        application.process_git_branch_events();
        application.process_workspace_file_events();
        application.process_workspace_search_events();
        application.process_explorer_watch_events();
        application.process_explorer_result();
        application.process_trust_events();
        application.maybe_prompt_for_unseen_workspace();
        application.process_configuration_result();
        application.process_theme_results();
        application.process_workspace_approval();
        application.process_tool_progress();
        application.process_tool_results();
        application.process_tool_version_results();
        application.process_lsp_project_root();
        application.process_nix_lsp_workspace();
        application.process_dap_project_root();
        application.process_lsp_events();
        application.process_lsp_retry();
        application.process_format_on_save_deadline();
        application.process_disk_changes();
        application.restart_lsp_if_due();
        application.process_dap_events();
        application.process_execution_preflight();
        application.process_pty_events();
        application.process_agent_events();
        application.process_pull_request_events();
        application.process_remote_opens();
        application.process_plugin_storage_events();
        application.process_plugin_events();
        application.process_registry_events();
        application.process_package_events();
        application.process_job_events();
        application.process_large_file_modes();
        application.process_syntax();
        application.process_persistence_events();
        application.sync_lsp_document();
        application.process_lsp_inlay_refresh();
        application.process_lsp_breadcrumbs();
        application.sync_lsp_status_ui();
        application.sync_status_summary_ui();
        application.update_lsp_progress_animation(Instant::now());
        application.flush_keymap_timeout();
        application.update_key_hint_ui();
        application.update_notification_timer();
        application.update_live_crash_snapshot();
        application.reconcile_editor_windows();
        application.reconcile_git_signs();
        application.refresh_plugin_host_events();
        if let Some(text) = application.process_clipboard() {
            terminal.copy_to_clipboard(&text)?;
        }
        terminal.set_mouse(application.configuration.settings().ui.mouse)?;
        terminal.set_color_mode(application.configuration.settings().ui.color_mode);
        terminal.set_theme(application.configuration.theme());
        application
            .notices
            .set_terminal_capabilities(terminal.capability_summary());
        // The frame is a pure function of these, so it is only rebuilt when one moved.
        let inputs = (
            application.state.revision,
            application.ui.revision,
            application.configuration.binding_revision(),
            application.window_session.width(),
            application.window_session.height(),
        );
        let holding = application.hold_first_frame();
        let painted = application.draw_before_startup_reads(|application| {
            if holding {
                Ok(false)
            } else if input_arrived || terminal.needs_full_draw() || drawn != Some(inputs) {
                drawn = Some(inputs);
                let frame = render_frame(
                    &application.state,
                    &application.ui,
                    application.configuration.settings(),
                    application.configuration.dashboard(),
                    application.window_session.width(),
                    application.window_session.height(),
                );
                terminal.draw(&frame)
            } else {
                Ok(false)
            }
        })?;
        input_arrived &= holding;
        application.schedule_recovery_work();
        still_polls = if painted {
            0
        } else {
            still_polls.saturating_add(1)
        };
        let poll = Duration::from_millis(EVENT_POLL_MS);
        let poll = if still_polls < IDLE_AFTER {
            poll
        } else {
            poll.max(IDLE_POLL)
        };
        let poll = application
            .terminal_session
            .sync_deadline()
            .map_or(poll, |deadline| {
                poll.min(deadline.saturating_duration_since(std::time::Instant::now()))
            });
        if let Some(event) = terminal.poll_event(poll)? {
            still_polls = 0;
            input_arrived = true;
            if matches!(
                event,
                UiEvent::Resize(_, _) | UiEvent::FocusGained | UiEvent::FocusLost
            ) {
                terminal.invalidate();
            }
            application.handle_event(event);
        }
        application.tutor_tick();
    }
    drop(terminal);
    application.finish_shutdown()
}

impl InteractiveApplication {
    fn process_clipboard(&mut self) -> Option<String> {
        self.process_clipboard_paste();
        let generation = self.state.registers.generation();
        let output = if self.clipboard_shared() {
            self.clipboard.push(&mut self.state.value.registers)
        } else {
            None
        };
        if self.state.registers.generation() != generation {
            self.state.revision = self.state.revision.wrapping_add(1);
        }
        output
    }
}

fn mode_name(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Normal => "normal",
        EditorMode::Insert => "insert",
        EditorMode::Replace => "replace",
        EditorMode::VisualChar => "visual_char",
        EditorMode::VisualLine => "visual_line",
        EditorMode::VisualBlock => "visual_block",
        EditorMode::Multi => "multi",
        EditorMode::OperatorPending => "operator_pending",
        EditorMode::CommandLine => "command_line",
        EditorMode::Prompt => "prompt",
    }
}

fn command(id: &str) -> Result<CommandInvocation, CommandError> {
    CommandInvocation::new(id, InvocationSource::Keymap)
}

#[derive(Debug, Error)]
pub enum InteractiveError {
    #[error(transparent)]
    Application(#[from] ApplicationError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Terminal(#[from] TerminalError),
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error(transparent)]
    Encoding(#[from] crate::editor::text::EncodingError),
}

#[cfg(test)]
#[path = "../tests/mod.rs"]
mod tests;

mod render;
mod shutdown;
mod startup;
pub(super) mod tutor;
