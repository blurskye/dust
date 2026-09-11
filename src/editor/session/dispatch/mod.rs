use super::configuration::{self, ConfigurationSession};
use super::documents::{DocumentEvent, DocumentSession, LargeFileOverride, mode_name_large};
use super::explorer::explorer_watch_policy;
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
    EditorWindowBridge, NativeDescriptions, NativeSurfaceSignature, NativeSurfaceTarget,
    ProjectionRequest, RestoreWindowPolicy, WindowCandidate, WindowSession,
};
use configuration::*;
#[cfg(test)]
use configuration::{
    KeymapOperation, compile_keymaps, compile_keymaps_with_plugin_catalog, reload_keymaps,
};
#[path = "../commands/mod.rs"]
pub(super) mod commands;
use crate::editor::session::debugger::DebugSession;
mod effects;
mod events;

#[cfg(test)]
use super::persistence::capture_persisted_layout;
use super::syntax::{IndentError, SyntaxDocument, SyntaxSession, semantic_role};
use super::windows::rect_contains_point;
#[cfg(test)]
use crate::editor::transaction::build_restored_layout;
#[path = "../state.rs"]
mod state;
use crate::editor::session::tasks::{AdapterContext, RunningTask};
use state::*;

pub use commands::{
    AppEffect, DiagnosticFilter, LayoutAction, PickerKind, PromptMove, register_app_commands,
};

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::DeferredExecution;
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
use crate::language::syntax::{
    SyntaxLanguage, SyntaxSupport, builtin_languages, language_for_path,
};
use crate::persistence::{
    PersistedLargeFileOverride, PersistedLocation, PersistedSession, PersistedState,
    PersistedWindowRule, PersistencePolicy, PersistenceService, STATE_VERSION,
};
use crate::process::PtyId;
use crate::telemetry::{LiveCrashSnapshot, LogLevel};
use crate::tools::{
    NIX_LSP_NIXPKGS_REVISION, PackageOperation, RegistryRequest, RegistryResult, RegistryService,
    ToolDefinition, ToolKind, ToolOperation, ToolStatus, discover_executable, is_nixos_host,
    managed_tool_executable, managed_tool_installed_version, nix_lsp_launch_environment,
    nix_lsp_profile_installed, nix_lsp_workspace_root,
};
use crate::ui::{
    CompletionItem, DocumentationMarkupKind, DocumentationPopupKind,
    EditorWindowBinding as UiEditorWindowBinding, EditorWindowLayoutState, KeyInputKind,
    LanguageCompletionState, NativeSurfaceWindowBinding, PickerSurfaceKind, PromptKind,
    TerminalError, TerminalOptions, TerminalPanelMode, TerminalSession, ToolMenuRow, ToolMenuState,
    ToolRuntimeOption, ToolRuntimePickerState, UiEvent, UiFold, UiInlayHint, UiState, UiSyntaxSpan,
    VisualRowAnchor, VisualRowModel, active_pane_viewport, editor_state_content_width,
    editor_state_gutter_width, editor_workspace_insets, explorer_panel_width, pane_viewports,
    render, snap_display_column, snap_display_column_forward, terminal_panel_geometry,
    terminal_tab_at_column,
};
use crate::workspace::{
    DiagnosticOwner, ExecutionSource, ExplorerKind, FileSnapshot, PickerAction, PickerItem,
    PickerItemId, PickerState, RootIdentity, WorkspaceEditDraft, WorkspaceId, WorkspaceState,
    WorkspaceTrust,
};
use slotmap::Key;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

use super::extensions::{PluginUiInteraction, WindowUpdate};
use crate::editor::{ApplicationError, StartupTarget, build_state};
use crate::ui::window::{
    CompositorProjection, DockEdge, SpatialDirection, WindowPlacement, WindowRect,
};

struct InteractiveApplication {
    notices: super::notifications::NoticeSession,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    state: EditorState,
    extension: super::extensions::ExtensionSession,
    workspace: super::workspace::WorkspaceSession,
    diagnostics: super::diagnostics::DiagnosticSession,
    workspace_edits: super::workspace_edits::WorkspaceEditSession,
    commands: CommandRegistry<AppEffect>,
    input: InputSession,
    configuration: ConfigurationSession,
    ui: UiState,
    documents: DocumentSession,
    search: super::search::SearchSession,
    should_quit: bool,
    window_session: WindowSession,
    tooling: super::tooling::ToolingSession,
    language: super::lsp::LspSession,

    debugger_session: DebugSession,
    recovery: super::recovery::RecoverySession,
    next_transaction: u64,
    git: super::git::GitSession,
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
            alternate_screen: terminal_ui.alternate_screen,
            bracketed_paste: terminal_ui.bracketed_paste,
            focus_events: terminal_ui.focus_events,
            mouse: terminal_ui.mouse,
        },
        application.configuration.theme().clone(),
    )?;
    application
        .notices
        .set_terminal_capabilities(terminal.capability_summary());
    application.window_session.observe_size(terminal.size()?);

    while !application.should_quit {
        application.process_vfs_results();
        application.process_git_status_events();
        application.process_git_hunk_events();
        application.process_git_sign_events();
        application.process_git_mutation_events();
        application.process_workspace_file_events();
        application.process_workspace_search_events();
        application.process_explorer_watch_events();
        application.process_explorer_result();
        application.process_trust_events();
        application.maybe_prompt_for_unseen_workspace();
        application.process_configuration_result();
        application.process_theme_results();
        application.process_tool_progress();
        application.process_tool_results();
        application.process_tool_version_results();
        application.process_lsp_project_root();
        application.process_nix_lsp_workspace();
        application.process_dap_project_root();
        application.process_lsp_events();
        application.restart_lsp_if_due();
        application.process_dap_events();
        application.process_execution_preflight();
        application.process_pty_events();
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
        application.sync_lsp_status_ui();
        application.sync_status_summary_ui();
        application.update_lsp_progress_animation();
        application.flush_keymap_timeout();
        application.update_key_hint_ui();
        application.update_notification_timer();
        application.update_live_crash_snapshot();
        application.reconcile_editor_windows();
        application.reconcile_git_signs();
        application.refresh_plugin_host_events();
        let frame = render(
            &application.state,
            &application.ui,
            application.configuration.settings(),
            application.configuration.dashboard(),
            application.window_session.width(),
            application.window_session.height(),
        );
        terminal.set_mouse(application.configuration.settings().ui.mouse)?;
        terminal.set_color_mode(application.configuration.settings().ui.color_mode);
        terminal.set_theme(application.configuration.theme().clone());
        application
            .notices
            .set_terminal_capabilities(terminal.capability_summary());
        terminal.draw(&frame)?;
        application.queue_startup_reads();
        application.schedule_recovery_work();
        if let Some(event) = terminal.poll_event(Duration::from_millis(
            application.configuration.settings().runtime.event_poll_ms,
        ))? {
            if matches!(
                event,
                UiEvent::Resize(_, _) | UiEvent::FocusGained | UiEvent::FocusLost
            ) {
                terminal.invalidate();
            }
            application.handle_event(event);
        }
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
