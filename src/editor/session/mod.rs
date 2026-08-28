mod configuration;
use configuration::*;
pub use configuration::{
    ConfigurationSources, KeymapInspection, KeymapOperation, ResolvedConfiguration,
    ResolvedKeymaps, compile_keymaps, compile_keymaps_with_plugin_catalog,
    compile_keymaps_with_plugin_index, reload_keymaps, validate_loaded_configuration,
};
mod commands;
mod debugger;
mod documents;
mod editing_input;
mod effects;
mod events;
mod explorer;
mod extensions;
mod health;
use health::*;
use persistence::*;
use syntax::*;
use window_management::*;
mod lsp;
mod persistence;
mod project;
mod prompts;
mod recovery;
mod state;
use state::*;
mod syntax;
mod tasks;
mod terminal;
mod terminal_service;
mod tooling;
mod tools;
mod viewport;
mod window_bridge;
mod window_management;
mod windowing;

pub use commands::{AppEffect, LayoutAction, PickerKind, PromptMove, register_app_commands};

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use crate::config::LoadedConfig;
use crate::debugger::{
    DapControl, DapEvent, DapResponse, DapService, DapSourceBreakpoint, DapStackFrame,
};
use crate::editor::input::{
    InputAction, InputContextLayer, InputContextRegistry, KeyEvent, KeymapLayer, KeymapResolver,
    MappingResolution, ModalResolver, parse_ex,
};
use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, DisplayWidthPolicy, PositionCodec, PositionEncoding,
    TextBuffer, TextEdit, TextEditSet, display_column_at, next_grapheme_boundary,
};
use crate::editor::{
    ArgumentSchema, Axis, CommandError, CommandId, CommandInvocation, CommandOutcome,
    CommandRegistry, CommandSource, CommandSpec, CommandValue, ContextRequirement, EditorMode,
    EditorOperation, EditorState, EditorTransaction, FocusDirection, InvocationSource,
    Precondition, RegisterShape, RegisterValue, TransactionId, UndoPolicy, ViewId,
    register_builtin_commands,
};
use crate::extensions::{
    PluginContributionCatalog, PluginEvent, PluginExecution, PluginHostEventQueue, PluginLimits,
    PluginLoadId, PluginManager, PluginProviderBroker, PluginProviderSessionStatus, PluginSnapshot,
    PluginStatus, PluginStorageResult, PluginStorageService, PluginSubscriptionKind, PluginUiEvent,
    PluginVirtualDocumentBroker,
};
use crate::files::{
    CheckedResourceIntent, DirectoryEntryKind, DirectoryService, ExplorerWatchBatch,
    ExplorerWatchEvent, ExplorerWatchGeneration, ExplorerWatchPolicy, ExplorerWatchService,
    ProjectRootResolution, ProjectRootService, RecoveryRecord, RequestId, ResourceIntentKind,
    VfsError, VfsResult, VfsWorker, WorkspaceFileEntry, WorkspaceFileEvent, WorkspaceFilePolicy,
    WorkspaceFileService, WorkspaceSearchEvent, WorkspaceSearchMatch, WorkspaceSearchPolicy,
    WorkspaceSearchService, WriteExpectation,
};
use crate::language::lsp::{
    LspCapabilities, LspCompletionItem, LspDocument, LspDocumentEdit, LspEvent, LspFeature,
    LspFeatureResult, LspMarkupContent, LspMarkupKind, LspPosition, LspProgressKind, LspRange,
    LspService, LspTextEdit, LspWorkspaceEdit, file_uri, language_id_for_path, path_from_file_uri,
};
use crate::language::syntax::{
    SyntaxDocumentId, SyntaxEvent, SyntaxLanguage, SyntaxPolicy, SyntaxResult, SyntaxService,
    SyntaxSupport, builtin_languages, language_for_path,
};
use crate::persistence::{
    PersistedBreakpoint, PersistedLargeFileOverride, PersistedLayout, PersistedLocation,
    PersistedMark, PersistedPane, PersistedSession, PersistedState, PersistedTab, PersistedWatch,
    PersistedWindowPlacement, PersistedWindowRule, PersistenceEvent, PersistenceId,
    PersistencePolicy, PersistenceService, STATE_VERSION, validate_window_rules,
};
use crate::process::{
    ExecutionPreflightEvent, ExecutionPreflightRequest, ExecutionPreflightService, JobEvent, JobId,
    JobPolicy, JobRequest, JobStream, JobSupervisor, PreflightId, PtyEvent, PtyId, PtyPolicy,
    PtyRequest, PtyService,
};
use crate::telemetry::{
    CrashSnapshotWriter, LiveCrashSnapshot, LogLevel, LogPolicy, StructuredLogs,
};
use crate::tools::{
    NIX_LSP_NIXPKGS_REVISION, NixWorkspaceService, PackageOperation, PackageService,
    PackageServiceEvent, RegistryPackageMetadata, RegistryPackageSummary, RegistryRequest,
    RegistryResult, RegistryService, ToolDefinition, ToolEntry, ToolInstaller, ToolKind,
    ToolOperation, ToolStatus, ToolVersionRequest, ToolVersionService, discover_executable,
    inspect_tools, is_nixos_host, managed_tool_executable, managed_tool_installed_version,
    nix_lsp_launch_environment, nix_lsp_launch_environment_for_workspace_at,
    nix_lsp_profile_installed, nix_lsp_shell, nix_lsp_tool_installed, nix_lsp_workspace_root,
    tool_launch_environment,
};
use crate::ui::{
    CompletionItem, CompositorLayoutState, ContentTabDiagnostic, ContentTabDiagnosticSeverity,
    ContentTabEntry, ContentTabIdentity, ContentTabKind, DashboardAction, DocumentationMarkupKind,
    DocumentationPopupKind, DocumentationPopupState, DocumentationSection,
    EditorWindowBinding as UiEditorWindowBinding, EditorWindowLayoutState, ExplorerSegmentRole,
    ExplorerUiRow, ExplorerUiSegment, ExplorerUiState, KeyHintUiRow, KeyHintUiState, KeyInputKind,
    LanguageCompletionState, NativeSurfaceWindowBinding, PickerRow, PickerSurfaceKind,
    PickerUiState, PromptKind, PromptState, PtyScreen, TerminalError, TerminalOptions,
    TerminalPanelMode, TerminalPanelState, TerminalSession, TerminalTabState, ToolMenuRow,
    ToolMenuState, ToolRuntimeOption, ToolRuntimePickerState, UiDiagnostic, UiEvent, UiFold,
    UiInlayHint, UiLspProgress, UiState, UiSyntaxSpan, VisualRowAnchor, VisualRowModel,
    active_pane_viewport, editor_state_content_width, editor_state_gutter_width,
    editor_workspace_insets, explorer_panel_width, pane_viewports, picker_geometry,
    prompt_geometry, render, snap_display_column, snap_display_column_forward,
    terminal_panel_geometry, terminal_tab_at_column,
};
use crate::workspace::{
    Diagnostic, DiagnosticOwner, DiagnosticSeverity, DiagnosticStore, ExecutionSource,
    ExplorerChild, ExplorerKind, ExplorerLoadState, ExplorerNodeId, ExplorerState, FileSnapshot,
    PickerAction, PickerItem, PickerItemId, PickerState, ProposalId, RootIdentity, TrustEvent,
    TrustStore, WorkspaceEditDraft, WorkspaceEditProposal, WorkspaceId, WorkspaceState,
    WorkspaceTrust, WorkspaceTrustService,
};
use slotmap::Key;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use self::{
    extensions::documents::VirtualDocumentEditorBridge,
    extensions::windows::{PluginUiInteraction, PluginWindowHost},
    window_bridge::{
        EditorWindowBridge, NativeSurfaceBridge, NativeSurfaceSignature, NativeSurfaceTarget,
    },
};
use crate::editor::{ApplicationError, StartupTarget, build_state};
use crate::ui::window::{
    CompositorProjection, ContentTypeId, DockEdge, SpatialDirection, WindowGraph, WindowPlacement,
    WindowRect, WindowTarget,
};

const LSP_INLAY_HINT_DEBOUNCE: Duration = Duration::from_millis(120);
const LSP_INLAY_HINT_EMPTY_RETRY_BASE: Duration = Duration::from_millis(500);
const LSP_INLAY_HINT_WARMUP_RETRY_LIMIT: u8 = 5;
const LSP_INLAY_HINT_MAX_STARTUP_DELAY: Duration = Duration::from_secs(8);
type ActiveBufferObservation = (u64, u64, usize, usize, usize, u8);
type PluginHostObservation = (u64, String, Option<ActiveBufferObservation>, u64);

struct InteractiveApplication {
    state: EditorState,
    windows: WindowGraph,
    editor_windows: EditorWindowBridge,
    surface_windows: NativeSurfaceBridge,
    plugin_windows: PluginWindowHost,
    pending_plugin_ui_events: BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>>,
    plugin_host_events: PluginHostEventQueue,
    plugin_host_event_observation: Option<PluginHostObservation>,
    plugin_storage: PluginStorageService,
    pending_plugin_storage_results: BTreeMap<(String, PluginLoadId), Vec<PluginStorageResult>>,
    plugin_providers: PluginProviderBroker,
    plugin_virtual_documents: PluginVirtualDocumentBroker,
    plugin_virtual_editor: VirtualDocumentEditorBridge,
    content_line_order: Vec<crate::editor::BufferId>,
    content_line_pins: BTreeSet<crate::editor::BufferId>,
    content_line_order_revision: u64,
    content_line_diagnostic_revision: u64,
    content_line_projection_signature: Option<(u64, u64, u64, u64)>,
    active_plugin_picker: Option<u64>,
    terminal_set_revision: u64,
    workspace: WorkspaceState,
    diagnostics: DiagnosticStore,
    picker: Option<PickerState>,
    picker_surface: PickerSurfaceKind,
    picker_payloads: HashMap<PickerItemId, PickerPayload>,
    pending_workspace_proposal: Option<PendingWorkspaceProposal>,
    workspace_proposal: Option<crate::workspace::WorkspaceEditProposal>,
    workspace_proposal_response: Option<(String, crate::language::lsp::ServerRequestId)>,
    workspace_resource_intents: Vec<CheckedResourceIntent>,
    accepted_resource_intents: Vec<CheckedResourceIntent>,
    resource_apply_pending: HashMap<RequestId, CheckedResourceIntent>,
    next_proposal: u64,
    commands: CommandRegistry<AppEffect>,
    input: ModalResolver,
    configuration: ResolvedConfiguration,
    pending_keymap: Option<PendingKeymap>,
    ui: UiState,
    configuration_worker: crate::config::ConfigurationWorker,
    configuration_generation: u64,
    vfs: VfsWorker,
    pending_vfs: HashMap<RequestId, PendingVfs>,
    command_history: Vec<String>,
    search_history: Vec<String>,
    history_index: Option<usize>,
    previous_mode: Option<(ViewId, EditorMode)>,
    last_search_forward: bool,
    last_change: Vec<CommandInvocation>,
    pending_change: Option<Vec<CommandInvocation>>,
    recording_macro: Option<(char, Vec<CommandInvocation>)>,
    last_macro: Option<char>,
    replay_depth: usize,
    replay_steps: usize,
    should_quit: bool,
    terminal_width: u16,
    terminal_height: u16,
    mouse_position: Option<(u16, u16)>,
    mouse_selection_anchor: Option<(ViewId, ByteOffset)>,
    observed_notification: Option<String>,
    notification_deadline: Option<Instant>,
    completion_generation: u64,
    startup_reads: VecDeque<(PathBuf, bool)>,
    startup_reads_pending: usize,
    startup_primary_buffer: Option<crate::editor::BufferId>,
    tools: Vec<ToolEntry>,
    /// Catalog LSP IDs explicitly switched from PATH preference to the verified
    /// upstream artifact processed by Dust's private Nix compatibility runtime.
    lsp_nix_workspace_preferred: BTreeSet<String>,
    /// Catalog LSP IDs explicitly switched to Dust's direct managed artifact.
    lsp_managed_preferred: BTreeSet<String>,
    /// Exact catalog-managed version selected per LSP ID.
    lsp_managed_versions: BTreeMap<String, String>,
    tool_installer: ToolInstaller,
    tool_version_service: ToolVersionService,
    tool_version_pending: BTreeSet<String>,
    tool_versions: BTreeMap<String, Result<String, String>>,
    lsp_last_errors: BTreeMap<String, String>,
    lsp: LspService,
    active_lsp_server: Option<String>,
    lsp_project_roots: ProjectRootService,
    pending_lsp_project_root: Option<(u64, PendingLspProjectRoot)>,
    nix_lsp_workspace: NixWorkspaceService,
    pending_nix_lsp_workspace: Option<(u64, PendingNixLspWorkspace)>,
    dap_project_roots: ProjectRootService,
    pending_dap_project_root: Option<(u64, PendingDapProjectRoot)>,
    lsp_autostart_observation: Option<LspAutostartObservation>,
    lsp_sessions: HashMap<String, LspRuntimeState>,
    lsp_session_synced: HashMap<(String, crate::editor::BufferId), BufferRevision>,
    lsp_synced_revisions: HashMap<crate::editor::BufferId, BufferRevision>,
    lsp_capabilities: Option<LspCapabilities>,
    lsp_request_generation: u64,
    lsp_inlay_request_generation: u64,
    lsp_inlay_request_owner: Option<(u64, LspProjectionOwner)>,
    lsp_inlay_refresh_due: Option<Instant>,
    lsp_inlay_empty_retries: u8,
    lsp_logs: VecDeque<String>,
    lsp_progress_epoch: Instant,
    lsp_completion_items: Vec<LspCompletionItem>,
    snippet_session: Option<SnippetSession>,
    lsp_selection_ranges: Vec<ByteRange>,
    lsp_semantic_raw: HashMap<(String, String), (Option<String>, Vec<u32>)>,
    lsp_pull_ids: HashMap<(String, String), String>,
    lsp_projection_owner: Option<LspProjectionOwner>,
    lsp_request_owner: Option<(u64, LspProjectionOwner)>,
    lsp_completion_owner: Option<(u64, LspProjectionOwner)>,

    dap: DapService,
    dap_responses: VecDeque<(u64, DapResponse)>,
    dap_console: VecDeque<(String, String)>,
    dap_breakpoints: BTreeMap<PathBuf, Vec<DapSourceBreakpoint>>,
    dap_watches: Vec<String>,
    active_debug_configuration: Option<String>,
    active_debug_terminal_profile: Option<String>,
    dap_terminal_responses: HashMap<PtyId, i64>,
    recovery_directory: PathBuf,
    recovery_records: Vec<(PathBuf, RecoveryRecord)>,
    recovery_revisions: HashMap<crate::editor::BufferId, BufferRevision>,
    recovery_pruned_revisions: HashMap<crate::editor::BufferId, BufferRevision>,
    recovery_scheduled: HashMap<crate::editor::BufferId, (BufferRevision, Instant)>,
    recovery_discovery_queued: bool,
    recovery_cleanup_queued: bool,
    terminal_capabilities: String,
    next_transaction: u64,
    workspace_search: WorkspaceSearchService,
    git_status: crate::workspace::git::GitStatusService,
    git_status_generation: Option<crate::workspace::git::GitStatusGeneration>,
    git_status_root_revision: Option<u64>,
    git_status_snapshot: Option<crate::workspace::git::GitStatusSnapshot>,
    git_status_decorations: BTreeMap<PathBuf, crate::workspace::git::GitFileStatus>,
    git_status_silent: bool,
    git_status_silent_attempted: Option<(u64, PathBuf)>,
    git_hunks: crate::workspace::git::GitHunkService,
    git_hunk_generation: Option<crate::workspace::git::GitHunkGeneration>,
    git_hunk_expected: Option<(u64, u64, crate::workspace::git::GitRepositoryId, PathBuf)>,
    git_hunk_snapshot: Option<crate::workspace::git::GitHunkSnapshot>,
    git_sign_hunks: crate::workspace::git::GitHunkService,
    git_sign_generation: Option<crate::workspace::git::GitHunkGeneration>,
    git_sign_owner: Option<(
        u64,
        u64,
        crate::workspace::git::GitRepositoryId,
        PathBuf,
        crate::editor::BufferId,
        u64,
    )>,
    selected_git_hunk: Option<(
        crate::workspace::git::GitHunkSnapshot,
        crate::workspace::git::GitHunk,
        PathBuf,
    )>,
    git_mutation: crate::workspace::git::GitMutationService,
    git_mutation_generation: Option<crate::workspace::git::GitMutationGeneration>,
    workspace_files: WorkspaceFileService,
    workspace_file_generation: Option<u64>,
    workspace_file_root_revision: Option<u64>,
    workspace_file_entries: Vec<WorkspaceFileEntry>,
    workspace_search_generation: Option<u64>,
    workspace_search_workflow: Option<crate::workspace::search_workflow::SearchWorkflowBroker>,
    workspace_search_sequence: u64,
    workspace_search_matches: Vec<WorkspaceSearchMatch>,
    workspace_search_intent: Option<WorkspaceSearchIntent>,
    trust_service: WorkspaceTrustService,
    trust_store: TrustStore,
    trust_state_path: PathBuf,
    trust_load_generation: Option<u64>,
    trust_identity_generation: Option<u64>,
    trust_persist_generation: Option<u64>,
    trust_persist_target: Option<WorkspaceTrust>,
    trust_database_ready: bool,
    trust_identity_ready: bool,
    execution_validation: Option<PendingExecutionValidation>,
    execution_validation_generations: HashMap<u64, (crate::workspace::RootId, RootIdentity)>,
    execution_revalidated: bool,
    safe_mode: bool,
    pending_root_operation: Option<PendingRootOperation>,
    workspace_search_root_revision: Option<u64>,
    workspace_proposal_root_revision: Option<u64>,
    directory_service: DirectoryService,
    explorer: ExplorerState,
    explorer_pending: Option<(ExplorerNodeId, u64, u64)>,
    explorer_hidden: bool,
    explorer_ignored: bool,
    explorer_preview_path: Option<PathBuf>,
    explorer_preview_lines: Vec<String>,
    explorer_preview_generation: u64,
    explorer_follow_path: Option<PathBuf>,
    explorer_watch: ExplorerWatchService,
    explorer_watch_generation: Option<ExplorerWatchGeneration>,
    explorer_watch_observation: Option<(u64, u64)>,
    explorer_watch_reload_queue: VecDeque<PathBuf>,
    syntax: SyntaxService,
    syntax_documents: HashMap<crate::editor::BufferId, SyntaxDocumentId>,
    syntax_submitted: HashMap<crate::editor::BufferId, (BufferRevision, u64, SyntaxLanguage)>,
    syntax_accepted: HashMap<crate::editor::BufferId, SyntaxResult>,
    syntax_failures: HashMap<crate::editor::BufferId, SyntaxFailure>,
    syntax_due: HashMap<crate::editor::BufferId, (BufferRevision, Instant)>,
    next_syntax_document: u64,
    next_syntax_generation: u64,
    persistence: PersistenceService,
    persistence_path: PathBuf,
    persistence_pending: Option<PersistenceId>,
    session_restore: Option<PendingSessionRestore>,
    recent_files: Vec<PathBuf>,
    marks: Vec<PersistedMark>,
    jump_list: Vec<PersistedLocation>,
    change_list: Vec<PersistedLocation>,
    large_file_overrides: HashMap<crate::editor::BufferId, LargeFileOverride>,
    persisted_large_file_overrides: HashMap<PathBuf, LargeFileOverride>,
    jobs: JobSupervisor,
    job_tasks: HashMap<JobId, RunningTask>,
    task_output: HashMap<String, Vec<u8>>,
    task_output_rows: HashMap<String, Vec<TaskOutputRecord>>,
    task_truncated: HashSet<JobId>,
    task_status: HashMap<String, String>,
    test_results: HashMap<String, Vec<TestCaseResult>>,
    matcher_cache: HashMap<String, regex::Regex>,
    next_linter_generation: u64,
    pending_linter_builds: HashMap<u64, PendingLinterBuild>,
    logs: StructuredLogs,
    crash_snapshot: CrashSnapshotWriter,
    next_crash_snapshot: Instant,
    plugins: PluginManager,
    plugin_contributions: PluginContributionCatalog,
    pending_index_activation: BTreeMap<String, (PluginLoadId, String)>,
    rejected_plugin_loads: BTreeSet<(String, PluginLoadId)>,
    packages: PackageService,
    registry: RegistryService,
    registry_intents: HashMap<u64, bool>,
    pending_registry_artifact: Option<(RegistryPackageMetadata, Vec<u8>)>,
    package_prepared: HashMap<String, (PluginLoadId, u64)>,
    package_commits: HashMap<u64, (String, PluginLoadId)>,
    package_startup_pending: HashMap<String, PluginLoadId>,
    package_managed_plugins: HashMap<String, PluginLoadId>,
    package_startup_request: Option<u64>,
    package_startup_rollback: Option<u64>,
    package_startup_rollback_attempted: bool,
    plugin_worker_reported: bool,
    ptys: PtyService,
    terminals: HashMap<PtyId, TerminalRuntime>,
    active_terminal: Option<PtyId>,
    terminal_focused: bool,
    terminal_panel_height: u16,
    execution_preflight: ExecutionPreflightService,
    pending_execution_preflight: HashMap<PreflightId, PendingExecutionPreflight>,
}

pub fn run_interactive(
    startup: StartupTarget,
    loaded_config: LoadedConfig,
    safe_mode: bool,
) -> Result<(), InteractiveError> {
    let mut state = build_state(&startup);
    let startup_reads = match &startup {
        StartupTarget::Dashboard { .. } => VecDeque::new(),
        StartupTarget::File {
            path,
            additional_files,
            ..
        } => std::iter::once((path.clone(), true))
            .chain(additional_files.iter().cloned().map(|path| (path, false)))
            .collect(),
    };
    let mut commands = CommandRegistry::default();
    register_builtin_commands(&mut commands)?;
    register_app_commands(&mut commands)?;
    let use_user_configuration = loaded_config.user_configuration;
    let (configuration, config_diagnostic) =
        match resolve_configuration(&commands, loaded_config, true) {
            Ok(resolved) => resolved,
            Err(diagnostic) => {
                let bundled = crate::config::load_bundled_config();
                let (resolved, _) = resolve_configuration(&commands, bundled, true)
                    .map_err(InteractiveError::Configuration)?;
                let message = resolved.settings.messages["startup_fallback"]
                    .replace("{diagnostic}", &diagnostic);
                (resolved, Some(message))
            }
        };
    let resolved_dashboard_actions = dashboard_actions(&configuration);
    state.policy = crate::editor::EditingPolicy {
        tab_width: configuration.settings.editor.tab_width,
        indent_width: configuration.settings.editor.indent_width,
        expand_tabs: configuration.settings.editor.expand_tabs,
        undo_break_on_text_commit: configuration.settings.editor.undo_break_on_text_commit,
        registers_enabled: configuration.settings.editor.registers.storage
            == crate::config::RegisterStorage::Internal,
        mirror_named_to_unnamed: configuration
            .settings
            .editor
            .registers
            .mirror_named_to_unnamed,
        ambiguous_width_wide: configuration.settings.editor.ambiguous_width
            == crate::config::AmbiguousWidth::Wide,
    };
    let dashboard_visible = matches!(startup, StartupTarget::Dashboard { .. })
        && configuration.settings.startup.show_dashboard;
    if matches!(startup, StartupTarget::Dashboard { .. }) && !dashboard_visible {
        state.open_text(TextBuffer::from_text(""), None);
    }
    let workspace = WorkspaceState::new(
        WorkspaceId(1),
        [(
            "workspace".into(),
            RootIdentity {
                canonical_path: state.workspace_root.clone(),
                filesystem_identity: None,
            },
        )],
    )
    .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let diagnostics = DiagnosticStore::new(
        configuration.settings.workspace.maximum_diagnostics,
        configuration
            .settings
            .workspace
            .maximum_diagnostics_per_owner_uri,
    );
    let vfs_queue_capacity = configuration.settings.runtime.vfs_queue_capacity;
    let syntax_queue_capacity = configuration.settings.syntax.queue_capacity;
    let persistence_queue_capacity = configuration.settings.persistence.queue_capacity;
    let process_event_capacity = configuration.settings.processes.event_capacity;
    let persistence_path =
        crate::config::user_state_directory().join(&configuration.settings.persistence.state_file);
    let trust_state_path = crate::config::user_state_directory().join("trusted-workspaces.toml");
    let mut trust_service =
        WorkspaceTrustService::start(configuration.settings.workspace.trust_state_queue_capacity);
    let trust_load_generation = trust_service.load(trust_state_path.clone()).ok();
    let trust_identity_generation = trust_service
        .inspect_root(state.workspace_root.clone())
        .ok();
    let config_watch = configuration.settings.config_watch.clone();
    let watched_theme = configuration.settings.ui.theme.clone();
    let logs = make_logs(&configuration.settings, true);
    let plugin_queue_capacity = configuration.settings.plugins.queue_capacity;
    let plugin_storage_root = crate::config::user_state_directory().join("plugin-data-v1");
    let registry = RegistryService::start(
        configuration.settings.packages.registry.clone(),
        crate::config::user_state_directory().join("packages/registry-cache"),
    )
    .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let pty_queue_capacity = configuration.settings.terminals.queue_capacity;
    let terminal_panel_height = configuration.settings.ui.terminal_panel.height;
    let explorer_watch = ExplorerWatchService::start(explorer_watch_policy(
        &configuration.settings.ui.explorer.watch,
    ))
    .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let mut application = InteractiveApplication {
        state,
        windows: WindowGraph::default(),
        editor_windows: EditorWindowBridge::default(),
        surface_windows: NativeSurfaceBridge::default(),
        plugin_windows: PluginWindowHost::default(),
        pending_plugin_ui_events: BTreeMap::new(),
        plugin_host_events: PluginHostEventQueue::default(),
        plugin_host_event_observation: None,
        plugin_storage: PluginStorageService::start(plugin_storage_root, plugin_queue_capacity),
        pending_plugin_storage_results: BTreeMap::new(),
        plugin_providers: PluginProviderBroker::default(),
        plugin_virtual_documents: PluginVirtualDocumentBroker::default(),
        plugin_virtual_editor: VirtualDocumentEditorBridge::default(),
        content_line_order: Vec::new(),
        content_line_pins: BTreeSet::new(),
        content_line_order_revision: 0,
        content_line_diagnostic_revision: 0,
        content_line_projection_signature: None,
        active_plugin_picker: None,
        terminal_set_revision: 0,
        workspace,
        diagnostics,
        picker: None,
        picker_surface: PickerSurfaceKind::Generic,
        picker_payloads: HashMap::new(),
        pending_workspace_proposal: None,
        workspace_proposal: None,
        workspace_proposal_response: None,
        workspace_resource_intents: Vec::new(),
        accepted_resource_intents: Vec::new(),
        resource_apply_pending: HashMap::new(),
        next_proposal: 1,
        commands,
        input: ModalResolver::with_count_limit(configuration.settings.input.count_limit),
        configuration,
        pending_keymap: None,
        ui: UiState {
            dashboard: dashboard_visible,
            config_diagnostic,
            dashboard_actions: resolved_dashboard_actions,
            ..UiState::default()
        },
        configuration_worker: crate::config::ConfigurationWorker::start(
            use_user_configuration,
            config_watch,
            watched_theme,
        ),
        configuration_generation: 0,
        vfs: VfsWorker::start(vfs_queue_capacity),
        pending_vfs: HashMap::new(),
        command_history: Vec::new(),
        search_history: Vec::new(),
        history_index: None,
        previous_mode: None,
        last_search_forward: true,
        last_change: Vec::new(),
        pending_change: None,
        recording_macro: None,
        last_macro: None,
        replay_depth: 0,
        replay_steps: 0,
        should_quit: false,
        terminal_width: 0,
        terminal_height: 0,
        mouse_position: None,
        mouse_selection_anchor: None,
        observed_notification: None,
        notification_deadline: None,
        completion_generation: 0,
        startup_reads,
        startup_reads_pending: 0,
        startup_primary_buffer: None,
        tools: inspect_tools(),
        lsp_nix_workspace_preferred: BTreeSet::new(),
        lsp_managed_preferred: BTreeSet::new(),
        lsp_managed_versions: BTreeMap::new(),
        tool_installer: ToolInstaller::start(),
        tool_version_service: ToolVersionService::start(),
        tool_version_pending: BTreeSet::new(),
        tool_versions: BTreeMap::new(),
        lsp_last_errors: BTreeMap::new(),
        lsp: LspService::new(),
        active_lsp_server: None,
        lsp_project_roots: ProjectRootService::default(),
        pending_lsp_project_root: None,
        nix_lsp_workspace: NixWorkspaceService::default(),
        pending_nix_lsp_workspace: None,
        dap_project_roots: ProjectRootService::default(),
        pending_dap_project_root: None,
        lsp_autostart_observation: None,
        lsp_sessions: HashMap::new(),
        lsp_session_synced: HashMap::new(),
        lsp_synced_revisions: HashMap::new(),
        lsp_capabilities: None,
        lsp_request_generation: 0,
        lsp_inlay_request_generation: 0,
        lsp_inlay_request_owner: None,
        lsp_inlay_refresh_due: None,
        lsp_inlay_empty_retries: 0,
        lsp_logs: VecDeque::new(),
        lsp_progress_epoch: Instant::now(),
        lsp_completion_items: Vec::new(),
        snippet_session: None,
        lsp_selection_ranges: Vec::new(),
        lsp_semantic_raw: HashMap::new(),
        lsp_pull_ids: HashMap::new(),
        lsp_projection_owner: None,
        lsp_request_owner: None,
        lsp_completion_owner: None,
        dap: DapService::new(),
        dap_responses: VecDeque::new(),
        dap_console: VecDeque::new(),
        dap_breakpoints: BTreeMap::new(),
        dap_watches: Vec::new(),
        active_debug_configuration: None,
        active_debug_terminal_profile: None,
        dap_terminal_responses: HashMap::new(),
        recovery_directory: crate::config::user_state_directory().join("recovery"),
        recovery_records: Vec::new(),
        recovery_revisions: HashMap::new(),
        recovery_pruned_revisions: HashMap::new(),
        recovery_scheduled: HashMap::new(),
        recovery_discovery_queued: false,
        recovery_cleanup_queued: false,
        terminal_capabilities: "pending".into(),
        next_transaction: 1_000_000,
        workspace_search: WorkspaceSearchService::default(),
        git_status: crate::workspace::git::GitStatusService::default(),
        git_status_generation: None,
        git_status_root_revision: None,
        git_status_snapshot: None,
        git_status_decorations: BTreeMap::new(),
        git_status_silent: false,
        git_status_silent_attempted: None,
        git_hunks: crate::workspace::git::GitHunkService::default(),
        git_hunk_generation: None,
        git_hunk_expected: None,
        git_hunk_snapshot: None,
        git_sign_hunks: crate::workspace::git::GitHunkService::default(),
        git_sign_generation: None,
        git_sign_owner: None,
        selected_git_hunk: None,
        git_mutation: crate::workspace::git::GitMutationService::default(),
        git_mutation_generation: None,
        workspace_files: WorkspaceFileService::default(),
        workspace_file_generation: None,
        workspace_file_root_revision: None,
        workspace_file_entries: Vec::new(),
        workspace_search_generation: None,
        workspace_search_workflow: None,
        workspace_search_sequence: 0,
        workspace_search_matches: Vec::new(),
        workspace_search_intent: None,
        trust_service,
        trust_store: TrustStore::default(),
        trust_state_path,
        trust_load_generation,
        trust_identity_generation,
        trust_persist_generation: None,
        trust_persist_target: None,
        trust_database_ready: false,
        trust_identity_ready: false,
        execution_validation: None,
        execution_validation_generations: HashMap::new(),
        execution_revalidated: false,
        safe_mode,
        pending_root_operation: None,
        workspace_search_root_revision: None,
        workspace_proposal_root_revision: None,
        directory_service: DirectoryService::default(),
        explorer: ExplorerState::default(),
        explorer_pending: None,
        explorer_hidden: false,
        explorer_ignored: false,
        explorer_preview_path: None,
        explorer_preview_lines: Vec::new(),
        explorer_preview_generation: 0,
        explorer_follow_path: None,
        explorer_watch,
        explorer_watch_generation: None,
        explorer_watch_observation: None,
        explorer_watch_reload_queue: VecDeque::new(),
        syntax: SyntaxService::start(syntax_queue_capacity),
        syntax_documents: HashMap::new(),
        syntax_submitted: HashMap::new(),
        syntax_accepted: HashMap::new(),
        syntax_failures: HashMap::new(),
        syntax_due: HashMap::new(),
        next_syntax_document: 1,
        next_syntax_generation: 1,
        persistence: PersistenceService::start(persistence_queue_capacity),
        persistence_path,
        persistence_pending: None,
        session_restore: None,
        recent_files: Vec::new(),
        marks: Vec::new(),
        jump_list: Vec::new(),
        change_list: Vec::new(),
        large_file_overrides: HashMap::new(),
        persisted_large_file_overrides: HashMap::new(),
        jobs: JobSupervisor::new(process_event_capacity),
        job_tasks: HashMap::new(),
        task_output: HashMap::new(),
        task_output_rows: HashMap::new(),
        task_truncated: HashSet::new(),
        task_status: HashMap::new(),
        test_results: HashMap::new(),
        matcher_cache: HashMap::new(),
        next_linter_generation: 1,
        pending_linter_builds: HashMap::new(),
        logs,
        crash_snapshot: CrashSnapshotWriter::start(
            crate::config::user_state_directory().join("live-health.json"),
        ),
        next_crash_snapshot: Instant::now(),
        plugins: PluginManager::start(plugin_queue_capacity),
        plugin_contributions: PluginContributionCatalog::default(),
        pending_index_activation: BTreeMap::new(),
        rejected_plugin_loads: BTreeSet::new(),
        registry,
        registry_intents: HashMap::new(),
        pending_registry_artifact: None,
        package_prepared: HashMap::new(),
        package_commits: HashMap::new(),
        package_startup_pending: HashMap::new(),
        package_managed_plugins: HashMap::new(),
        package_startup_request: None,
        package_startup_rollback: None,
        package_startup_rollback_attempted: false,
        plugin_worker_reported: false,
        packages: PackageService::start(
            crate::config::user_state_directory().join("packages"),
            8,
            16 * 1024 * 1024,
            64 * 1024 * 1024,
        ),
        ptys: PtyService::start(pty_queue_capacity),
        terminals: HashMap::new(),
        active_terminal: None,
        terminal_focused: false,
        terminal_panel_height,
        execution_preflight: ExecutionPreflightService::start(process_event_capacity),
        pending_execution_preflight: HashMap::new(),
    };
    let configured_placements =
        configured_window_placements(&application.configuration.settings.windows)
            .map_err(InteractiveError::Configuration)?;
    application
        .surface_windows
        .set_configured_placements(configured_placements)
        .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let crash_path = crate::config::user_state_directory().join("last-crash.json");
    if application.safe_mode {
        application.ui.notification =
            Some(application.message("safe_mode_status", &[("state", "active".into())]));
        application.ui.notification_error = true;
    } else if crash_path.is_file() {
        application.ui.notification = Some(application.message("previous_crash", &[]));
        application.ui.notification_error = true;
    }
    application.rebuild_matcher_cache();
    if !application.safe_mode {
        application.load_configured_plugins();
        application.package_startup_request = application
            .packages
            .request(PackageOperation::Activations)
            .ok();
    }
    application.logs.record(
        LogLevel::Info,
        "application",
        "startup",
        BTreeMap::from([
            ("safe_mode".into(), application.safe_mode.to_string()),
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
    if !application.safe_mode {
        application.load_persistence();
    }
    if !application.startup_reads.is_empty() {
        application.ui.notification = Some(application.message("opening_file", &[]));
        application.ui.notification_error = false;
    }
    let terminal_ui = &application.configuration.settings.ui;
    let mut terminal = TerminalSession::enter(
        TerminalOptions {
            color_mode: terminal_ui.color_mode,
            alternate_screen: terminal_ui.alternate_screen,
            bracketed_paste: terminal_ui.bracketed_paste,
            focus_events: terminal_ui.focus_events,
            mouse: terminal_ui.mouse,
        },
        application.configuration.theme.clone(),
    )?;
    application.terminal_capabilities = terminal.capability_summary();
    (application.terminal_width, application.terminal_height) = terminal.size()?;

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
        application.process_configuration_result();
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
            &application.configuration.settings,
            &application.configuration.dashboard,
            application.terminal_width,
            application.terminal_height,
        );
        terminal.set_mouse(application.configuration.settings.ui.mouse)?;
        terminal.set_color_mode(application.configuration.settings.ui.color_mode);
        terminal.set_theme(application.configuration.theme.clone());
        application.terminal_capabilities = terminal.capability_summary();
        terminal.draw(&frame)?;
        application.queue_startup_reads();
        application.schedule_recovery_work();
        if let Some(event) = terminal.poll_event(Duration::from_millis(
            application.configuration.settings.runtime.event_poll_ms,
        ))? {
            if matches!(event, UiEvent::FocusGained | UiEvent::FocusLost) {
                terminal.invalidate();
            }
            application.handle_event(event);
        }
    }
    drop(terminal);
    application.save_persistence();
    let persistence_deadline = Instant::now() + Duration::from_secs(2);
    while application.persistence_pending.is_some() && Instant::now() < persistence_deadline {
        application.process_persistence_events();
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn configured_context(
    contexts: &InputContextRegistry,
    context: &str,
    mode: &str,
) -> Result<String, String> {
    contexts
        .resolve(context, mode)
        .map_err(|error| error.to_string())
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
mod tests;
