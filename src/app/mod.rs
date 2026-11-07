//! Live editor composition and feature-private owners.
pub(crate) mod configuration;
mod debugger;
mod diagnostics;
mod documents;
mod explorer;
mod git;
mod input;
mod language;
mod notifications;
mod persistence;
mod plugins;
mod search;
mod syntax;
mod tasks;
mod terminal;
mod tools;
mod windows;
mod workspace;

#[derive(Clone, Debug)]
pub(in crate::app) enum TrustedAction {
    StartDebug(debugger::DebugIntent),
    InstallRegistry {
        publisher: String,
        name: String,
        version: String,
    },
    PrepareRegistryArtifact,
    InstallPackage {
        package_manifest: std::path::PathBuf,
        plugin_manifest: std::path::PathBuf,
        component: std::path::PathBuf,
        update: bool,
    },
    RunTask(String),
    OpenTerminal(terminal::TerminalIntent),
    OpenDapTerminal {
        intent: terminal::TerminalIntent,
        reply: crate::debugger::DapTerminalReply,
    },
    RunTest {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunLinter {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunFormatter {
        name: String,
        source: tasks::FormatterSource,
    },
}

pub use configuration::validate_loaded_configuration;

use crate::app::configuration::Configuration;
use crate::app::documents::Documents;
use crate::app::input::InputState;
use crate::app::windows::Windows;
pub(super) mod commands;
use crate::app::debugger::Debugger;
pub(super) mod agent;
mod remote;
mod settings;

use crate::app::syntax::Syntax;

pub use commands::AppEffect;

pub(crate) use commands::PickerKind;

pub(crate) use commands::register_app_commands;

use std::collections::BTreeMap;

use std::path::PathBuf;

use std::time::Duration;

use std::time::Instant;

use crate::config::LoadedConfig;
use crate::editor::CommandError;
use crate::editor::CommandInvocation;
use crate::editor::CommandRegistry;
use crate::editor::EditorMode;
use crate::editor::EditorState;
use crate::editor::InvocationSource;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteRange;

use crate::files::VfsError;
use crate::process::PtyId;
use crate::telemetry::LogLevel;
use crate::ui::TerminalError;
use crate::ui::TerminalOptions;
use crate::ui::TerminalSession;
use crate::ui::UiEvent;
use crate::ui::UiState;
use crate::ui::render as render_frame;
use thiserror::Error;

use crate::app::startup::ApplicationError;
use crate::app::startup::StartupTarget;

pub(in crate::app) const EVENT_POLL_MS: u64 = 16;
pub(in crate::app) const MACRO_RECURSION_LIMIT: usize = 16;
pub(in crate::app) const MACRO_STEP_LIMIT: usize = 10_000;
pub(in crate::app) const REPEAT_STEP_LIMIT: usize = 10_000;
pub(in crate::app) const HALF_PAGE_PERCENT: u8 = 50;
pub(in crate::app) const FULL_PAGE_PERCENT: u8 = 100;

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
    authority: crate::editor::CommandAuthority,
    request: Option<(String, u64)>,
    buffer: crate::editor::BufferId,
    source: crate::editor::text::TextVersion,
    path: Option<std::path::PathBuf>,
    formatter: Option<String>,
    started: bool,
    quit_after: bool,
    deadline: std::time::Instant,
}

struct App {
    format_on_save: std::collections::HashMap<crate::editor::BufferId, FormatOnSave>,
    pending_saves: std::collections::VecDeque<(
        crate::editor::BufferId,
        bool,
        crate::editor::CommandAuthority,
    )>,
    navigation_reads: std::collections::HashMap<
        crate::files::RequestId,
        (crate::app::windows::NavigationStamp, std::sync::Arc<()>),
    >,
    /// The last request someone asked for, until their next key.
    lsp_asked: Option<crate::language::lsp::LspFeature>,
    lsp_asked_request_id: Option<u64>,
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
        Option<crate::git::GitHeadId>,
    )>,
    /// What project replace is looking for, while it asks what to replace it with.
    replace_query: Option<String>,
    /// A `:s///c` asking about each match in turn.
    substitute_confirm: Option<crate::app::editing::substitute::SubstituteConfirm>,
    session_load_requested: bool,
    session_restore_notice: bool,
    notices: crate::app::notifications::Notifications,
    health: crate::app::health::Health,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    state: Tracked<EditorState>,
    extension: crate::app::plugins::Plugins,
    workspace: crate::app::workspace::Workspace,
    diagnostics: crate::app::diagnostics::Diagnostics,
    workspace_edits: crate::app::workspace::edits::WorkspaceEdits,
    commands: CommandRegistry<AppEffect>,
    effect_authority: crate::editor::CommandAuthority,
    effect_failed: bool,
    input: InputState,
    input_replacement: prompt::replacement::InputReplacement,
    configuration: Configuration,
    ui: Tracked<UiState>,
    documents: Documents,
    search: crate::app::search::Search,
    should_quit: bool,
    /// When the last key arrived, so automatic prompts wait until typing pauses.
    last_key_at: Option<Instant>,
    /// The `s` jump while its labels are shown.
    flash: Option<crate::app::editing::flash::FlashState>,
    /// The ACP agent session and its transcript panel.
    agent: agent::AgentPanel,
    pull_requests: crate::app::git::pull_requests::PullRequests,
    tutor: tutor::Tutor,
    /// Commands last run from the palette, newest first.
    palette_recent: Vec<String>,
    /// The split border being dragged with the mouse: the panes on its two sides.
    border_drag: Option<(crate::windows::WindowId, crate::windows::WindowId, bool)>,
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
    window_session: Windows,
    tooling: crate::app::tools::Tools,
    language: crate::app::language::LspSession,

    debugger_session: Debugger,
    debug_terminal: Option<PtyId>,
    recovery: crate::app::documents::recovery::Recovery,
    next_transaction: u64,
    git: crate::app::git::Git,
    clipboard: crate::app::editing::clipboard::Clipboard,
    macro_replays: Vec<(Vec<CommandInvocation>, usize, u64)>,
    macro_running: bool,
    explorer: crate::app::explorer::Explorer,
    syntax: Syntax,
    persistence: crate::app::persistence::Persistence,
    tasks: crate::app::tasks::Tasks,
    terminal_session: crate::app::terminal::Terminals,
}

pub fn run_interactive(
    startup: StartupTarget,
    loaded_config: LoadedConfig,
    safe_mode: bool,
) -> Result<(), InteractiveError> {
    let new_files = startup.new_files();
    let mut application = App::new(
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
        .configure_initial(
            &application.configuration.settings().windows,
            &mut application.state,
        )
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
    application.health.record(
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
        application.open_setup_on_first_start();
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
        .health
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
        application.process_input_replacement();
        application.process_select_replacement();
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
        application.update_yank_highlight();
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
            .health
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
                let config = application.input_render_config();
                let mut frame = render_frame(
                    &application.state,
                    &application.ui,
                    config.as_ref(),
                    application.configuration.dashboard(),
                    application.window_session.width(),
                    application.window_session.height(),
                );
                application.project_mode_cursor(&mut frame);
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
mod tests;

mod shutdown;
pub mod startup;
pub(super) mod tutor;

mod documentation;
mod editing;
pub mod events;
mod picker;
mod prompt;

mod help;

mod dashboard;

mod health;
