use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::PathBuf,
    str::FromStr,
    time::{Duration, Instant},
};

use crate::configuration::LoadedConfig;
use crate::debugging::{
    DapControl, DapEvent, DapResponse, DapService, DapSourceBreakpoint, DapStackFrame,
};
use crate::document::{
    BufferRevision, ByteOffset, ByteRange, DisplayWidthPolicy, PositionCodec, PositionEncoding,
    TextBuffer, TextEdit, TextEditSet, display_column_at, next_grapheme_boundary,
};
use crate::editing::{
    ArgumentSchema, CommandError, CommandId, CommandInvocation, CommandOutcome, CommandRegistry,
    CommandSource, CommandSpec, CommandValue, ContextRequirement, EditorMode, EditorOperation,
    EditorState, EditorTransaction, InvocationSource, Precondition, TransactionId, UndoPolicy,
    ViewId, register_builtin_commands,
};
use crate::filesystem::{
    CheckedResourceIntent, DirectoryEntryKind, DirectoryService, RecoveryRecord, RequestId,
    ResourceIntentKind, VfsError, VfsResult, VfsWorker, WorkspaceSearchEvent, WorkspaceSearchMatch,
    WorkspaceSearchPolicy, WorkspaceSearchService, WriteExpectation,
};
use crate::input::{
    InputAction, KeyEvent, KeymapLayer, KeymapResolver, MappingResolution, ModalResolver, parse_ex,
};
use crate::languages::{
    LspCapabilities, LspCompletionItem, LspDocument, LspDocumentEdit, LspEvent, LspFeature,
    LspFeatureResult, LspPosition, LspRange, LspService, LspTextEdit, LspWorkspaceEdit, file_uri,
    language_id_for_path, path_from_file_uri,
};
use crate::observability::{
    CrashSnapshotWriter, LiveCrashSnapshot, LogLevel, LogPolicy, StructuredLogs,
};
use crate::packages::{
    PackageOperation, PackageService, PackageServiceEvent, RegistryPackageMetadata,
    RegistryPackageSummary, RegistryRequest, RegistryResult, RegistryService, ToolDefinition,
    ToolEntry, ToolInstaller, ToolKind, ToolStatus, discover_executable, inspect_tools,
};
use crate::persistence::{
    PersistedBreakpoint, PersistedLargeFileOverride, PersistedLayout, PersistedLocation,
    PersistedMark, PersistedPane, PersistedSession, PersistedState, PersistedTab, PersistedWatch,
    PersistenceEvent, PersistenceId, PersistencePolicy, PersistenceService, STATE_VERSION,
};
use crate::plugins::{
    PluginEvent, PluginExecution, PluginLimits, PluginLoadId, PluginManager, PluginSnapshot,
    PluginStatus,
};
use crate::processes::{
    ExecutionPreflightEvent, ExecutionPreflightRequest, ExecutionPreflightService, JobEvent, JobId,
    JobPolicy, JobRequest, JobStream, JobSupervisor, PreflightId, PtyEvent, PtyId, PtyPolicy,
    PtyRequest, PtyService,
};
use crate::syntax::{
    SyntaxDocumentId, SyntaxEvent, SyntaxLanguage, SyntaxPolicy, SyntaxResult, SyntaxService,
    SyntaxSupport, builtin_languages, language_for_path,
};
use crate::terminal_ui::{
    CompletionItem, DashboardAction, KeyInputKind, LanguageCompletionState, PickerRow,
    PickerUiState, PromptKind, PromptState, PtyScreen, TerminalError, TerminalOptions,
    TerminalSession, ToolMenuRow, ToolMenuState, UiDiagnostic, UiEvent, UiFold, UiInlayHint,
    UiState, UiSyntaxSpan, render,
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

use crate::application::{
    AppEffect, ApplicationError, LayoutAction, PickerKind, PromptMove, StartupTarget, build_state,
    register_app_commands,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeymapInspection {
    pub context: String,
    pub command: String,
    pub arguments: BTreeMap<String, CommandValue>,
    pub keys: String,
    pub source: String,
}

#[derive(Clone, Default)]
pub struct ResolvedKeymaps {
    resolvers: HashMap<String, KeymapResolver>,
    reverse: HashMap<(String, String), String>,
    inspections: Vec<KeymapInspection>,
    conflicts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationSources {
    pub settings: PathBuf,
    pub keymaps: PathBuf,
    pub dashboard: PathBuf,
    pub theme: PathBuf,
}

#[derive(Clone)]
pub struct ResolvedConfiguration {
    pub settings: crate::configuration::Config,
    pub dashboard: crate::configuration::DashboardConfig,
    pub theme: crate::configuration::ThemeConfig,
    pub keymaps: ResolvedKeymaps,
    pub sources: ConfigurationSources,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub dashboard_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub theme_spans: BTreeMap<String, std::ops::Range<usize>>,
}

fn resolved_toml_value(mut value: toml::Value, segments: &[String]) -> Option<toml::Value> {
    for segment in segments {
        value = if let Some(table) = value.as_table() {
            table.get(segment)?.clone()
        } else if let Some(array) = value.as_array() {
            array.get(segment.parse::<usize>().ok()?)?.clone()
        } else {
            return None;
        };
    }
    Some(value)
}

impl ResolvedConfiguration {
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<crate::configuration::ConfigExplanation> {
        let (value, source, span, layered) = if let Some(key) = key.strip_prefix("theme.") {
            let segments = crate::configuration::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value = resolved_toml_value(toml::Value::try_from(&self.theme).ok()?, &segments)?;
            (
                value,
                &self.sources.theme,
                self.theme_spans.get(&canonical),
                false,
            )
        } else if let Some(key) = key.strip_prefix("dashboard.") {
            let segments = crate::configuration::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value =
                resolved_toml_value(toml::Value::try_from(&self.dashboard).ok()?, &segments)?;
            (
                value,
                &self.sources.dashboard,
                self.dashboard_spans.get(&canonical),
                false,
            )
        } else {
            let segments = crate::configuration::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value =
                resolved_toml_value(toml::Value::try_from(&self.settings).ok()?, &segments)?;
            let source = if self.user_setting_keys.contains(&canonical) {
                &self.sources.settings
            } else {
                // Inherited values continue to identify the bundled declaration.
                static BUNDLED_SETTINGS: std::sync::LazyLock<PathBuf> =
                    std::sync::LazyLock::new(|| PathBuf::from("<bundled:config>"));
                &BUNDLED_SETTINGS
            };
            (value, source, self.user_setting_spans.get(&canonical), true)
        };
        let mut final_source = source.display().to_string();
        if let Some(span) = span {
            final_source = format!("{final_source}:bytes {}..{}", span.start, span.end);
        }
        let bundled = final_source.starts_with("<bundled:");
        Some(crate::configuration::ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: if bundled {
                "bundled defaults".into()
            } else {
                final_source.clone()
            },
            contributors: if bundled {
                vec!["bundled defaults".into()]
            } else if layered {
                vec!["bundled defaults".into(), final_source]
            } else {
                vec![final_source]
            },
        })
    }
}

impl ResolvedKeymaps {
    pub(crate) fn resolve(&mut self, context: &str, key: KeyEvent) -> MappingResolution {
        self.resolvers
            .get_mut(context)
            .map_or(MappingResolution::Missing, |resolver| resolver.feed(key))
    }

    #[must_use]
    pub fn binding_for(&self, context: &str, command: &str) -> Option<&str> {
        self.reverse
            .iter()
            .find(|((mapped_context, fingerprint), _)| {
                mapped_context == context
                    && fingerprint
                        .strip_prefix(command)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('|'))
            })
            .map(|(_, keys)| keys.as_str())
    }

    #[must_use]
    pub fn inspect(&self, context: &str, command: &str) -> Option<KeymapInspection> {
        self.binding_for(context, command).and_then(|keys| {
            self.inspections
                .iter()
                .find(|inspection| {
                    inspection.context == context
                        && inspection.command == command
                        && inspection.keys == keys
                })
                .cloned()
        })
    }

    #[must_use]
    pub fn inspect_key(&self, context: &str, keys: &str) -> Option<KeymapInspection> {
        self.inspections
            .iter()
            .find(|inspection| inspection.context == context && inspection.keys == keys)
            .cloned()
    }

    #[must_use]
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    fn binding_for_action(
        &self,
        context: &str,
        command: &str,
        arguments: &std::collections::BTreeMap<String, toml::Value>,
    ) -> Option<&str> {
        let mut invocation = CommandInvocation::new(command, InvocationSource::Keymap).ok()?;
        for (name, value) in arguments {
            invocation
                .arguments
                .insert(name.clone(), configuration_value(value).ok()?);
        }
        self.reverse
            .get(&(context.to_owned(), invocation_fingerprint(&invocation)))
            .map(String::as_str)
    }
}

#[derive(Clone, Debug)]
enum AdapterContext {
    Plain,
    Formatter {
        buffer: crate::editing::BufferId,
        revision: BufferRevision,
        source: Vec<u8>,
    },
    Linter {
        matcher: crate::configuration::ProblemMatcherConfig,
    },
    Test {
        matcher: crate::configuration::ProblemMatcherConfig,
    },
}
enum PendingExecutionPreflight {
    ToolInstall {
        definition: ToolDefinition,
    },
    Task {
        requested: String,
        name: String,
        remaining: Vec<String>,
        task: crate::configuration::TaskConfig,
        adapter: AdapterContext,
    },
    Terminal {
        profile: String,
        config: crate::configuration::TerminalProfileConfig,
        dap_request: Option<i64>,
    },
    Lsp {
        session_key: String,
        definition: ToolDefinition,
        root_id: crate::workspace::RootId,
        buffer: crate::editing::BufferId,
        document: LspDocument,
        restart: bool,
    },
    DapToolAdapter {
        definition: ToolDefinition,
        program: PathBuf,
    },
    DapToolProgram {
        definition: ToolDefinition,
        adapter_executable: PathBuf,
        cwd: PathBuf,
        root_revision: u64,
    },
    DapAdapter {
        name: String,
        adapter: crate::configuration::DebugAdapterConfig,
        session: crate::configuration::DebugSessionConfig,
    },
    DapProgram {
        name: String,
        adapter: crate::configuration::DebugAdapterConfig,
        session: crate::configuration::DebugSessionConfig,
        adapter_executable: PathBuf,
        cwd: PathBuf,
        root_revision: u64,
    },
}

#[derive(Clone, Debug)]
struct TaskOutputRecord {
    stream: JobStream,
    text: String,
    truncated: bool,
}
#[derive(Clone, Debug)]
struct RawLinterDiagnostic {
    line: usize,
    column: usize,
    severity: DiagnosticSeverity,
    message: String,
}
struct PendingLinterBuild {
    owner: DiagnosticOwner,
    root_revision: u64,
    waiting: usize,
    failed: bool,
    grouped: BTreeMap<String, Vec<Diagnostic>>,
}

#[derive(Clone, Debug)]
struct MatcherSource {
    path: PathBuf,
    line: usize,
    column: usize,
}
#[derive(Clone, Debug)]
struct TestCaseResult {
    name: String,
    status: String,
    duration: String,
    message: String,
    source: Option<MatcherSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LspProjectionOwner {
    server: String,
    buffer: crate::editing::BufferId,
    view: ViewId,
    revision: BufferRevision,
    uri: String,
    language: String,
}
#[derive(Clone, Debug)]
struct LspRuntimeState {
    root: crate::workspace::RootId,
    root_path: PathBuf,
    server_id: String,
    definition: ToolDefinition,
    executable: PathBuf,
    capabilities: Option<LspCapabilities>,
    documents: HashMap<crate::editing::BufferId, LspDocument>,
    restart_count: u8,
    restart_due: Option<Instant>,
    circuit_open: bool,
    desired_running: bool,
    activate_on_restart: bool,
}

#[derive(Clone, Debug)]
struct SyntaxFailure {
    revision: BufferRevision,
    generation: u64,
    language: SyntaxLanguage,
    diagnostic: String,
}
struct TerminalRuntime {
    profile: String,
    root_revision: u64,
    screen: PtyScreen,
    running: bool,
    exit_code: Option<u32>,
    truncated: bool,
}

struct RunningTask {
    requested: String,
    name: String,
    root_revision: u64,
    remaining: Vec<String>,
    adapter: AdapterContext,
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LargeFileOverride {
    Auto,
    On,
    Off,
}

struct InteractiveApplication {
    state: EditorState,
    workspace: WorkspaceState,
    diagnostics: DiagnosticStore,
    picker: Option<PickerState>,
    picker_payloads: HashMap<PickerItemId, PickerPayload>,
    pending_workspace_proposal: Option<PendingWorkspaceProposal>,
    workspace_proposal: Option<crate::workspace::WorkspaceEditProposal>,
    workspace_proposal_response: Option<(String, crate::languages::ServerRequestId)>,
    workspace_resource_intents: Vec<CheckedResourceIntent>,
    accepted_resource_intents: Vec<CheckedResourceIntent>,
    resource_apply_pending: HashMap<RequestId, CheckedResourceIntent>,
    next_proposal: u64,
    commands: CommandRegistry<AppEffect>,
    input: ModalResolver,
    configuration: ResolvedConfiguration,
    pending_keymap: Option<(Instant, String)>,
    ui: UiState,
    configuration_worker: crate::configuration::ConfigurationWorker,
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
    startup_primary_buffer: Option<crate::editing::BufferId>,
    tools: Vec<ToolEntry>,
    tool_installer: ToolInstaller,
    lsp: LspService,
    active_lsp_server: Option<String>,
    lsp_sessions: HashMap<String, LspRuntimeState>,
    lsp_session_synced: HashMap<(String, crate::editing::BufferId), BufferRevision>,
    lsp_synced_revisions: HashMap<crate::editing::BufferId, BufferRevision>,
    lsp_capabilities: Option<LspCapabilities>,
    lsp_request_generation: u64,
    lsp_logs: VecDeque<String>,
    lsp_completion_items: Vec<LspCompletionItem>,
    snippet_session: Option<SnippetSession>,
    lsp_selection_ranges: Vec<ByteRange>,
    lsp_semantic_raw: HashMap<(String, String), (Option<String>, Vec<u32>)>,
    lsp_pull_ids: HashMap<(String, String), String>,
    lsp_projection_owner: Option<LspProjectionOwner>,
    lsp_request_owner: Option<(u64, LspProjectionOwner)>,

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
    recovery_revisions: HashMap<crate::editing::BufferId, BufferRevision>,
    recovery_pruned_revisions: HashMap<crate::editing::BufferId, BufferRevision>,
    recovery_scheduled: HashMap<crate::editing::BufferId, (BufferRevision, Instant)>,
    recovery_discovery_queued: bool,
    recovery_cleanup_queued: bool,
    terminal_capabilities: String,
    next_transaction: u64,
    workspace_search: WorkspaceSearchService,
    workspace_search_generation: Option<u64>,
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
    syntax: SyntaxService,
    syntax_documents: HashMap<crate::editing::BufferId, SyntaxDocumentId>,
    syntax_submitted: HashMap<crate::editing::BufferId, (BufferRevision, u64, SyntaxLanguage)>,
    syntax_accepted: HashMap<crate::editing::BufferId, SyntaxResult>,
    syntax_failures: HashMap<crate::editing::BufferId, SyntaxFailure>,
    syntax_due: HashMap<crate::editing::BufferId, (BufferRevision, Instant)>,
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
    large_file_overrides: HashMap<crate::editing::BufferId, LargeFileOverride>,
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
    execution_preflight: ExecutionPreflightService,
    pending_execution_preflight: HashMap<PreflightId, PendingExecutionPreflight>,
}

#[derive(Clone, Debug)]
struct PendingSessionRestore {
    session: PersistedSession,
    waiting: usize,
    loaded: HashMap<PathBuf, (PathBuf, TextBuffer)>,
    failed: bool,
}

#[derive(Clone)]
struct SnippetSession {
    buffer: crate::editing::BufferId,
    revision: BufferRevision,
    groups: Vec<Vec<ByteRange>>,
    current: usize,
}

#[derive(Clone, Debug)]
struct PendingWorkspaceProposal {
    id: crate::workspace::ProposalId,
    encoding: PositionEncoding,
    label: String,
    documents: Vec<crate::languages::LspDocumentEdit>,
    snapshots: Vec<crate::workspace::FileSnapshot>,
    creates: Vec<PathBuf>,
    resource_intents: Vec<CheckedResourceIntent>,
    resolved_paths: HashMap<PathBuf, PathBuf>,
    waiting: usize,
    response: Option<(String, crate::languages::ServerRequestId)>,
}

#[derive(Clone, Debug)]
enum DeferredExecution {
    InstallTool(String),
    StartLsp(Option<String>),
    RestartLsp(String),
    StartDap {
        requested: Option<String>,
        program: Option<PathBuf>,
    },
    StartDapConfiguration(String),
    InstallRegistry {
        publisher: String,
        name: String,
        version: String,
    },
    PrepareRegistryArtifact,
    InstallPackage {
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    },
    RunTask(String),
    OpenTerminal(String),
    OpenDapTerminal {
        profile: String,
        request_seq: i64,
    },
    RunTest {
        name: String,
        matcher: crate::configuration::ProblemMatcherConfig,
    },
    RunLinter {
        name: String,
        matcher: crate::configuration::ProblemMatcherConfig,
    },
    RunFormatter {
        name: String,
        buffer: crate::editing::BufferId,
        revision: BufferRevision,
        source: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct PendingRootOperation {
    generation: u64,
    root_revision: u64,
    requested_path: PathBuf,
}

#[derive(Clone, Debug)]
struct PendingExecutionValidation {
    action: DeferredExecution,
    root_revision: u64,
    failed: bool,
}

enum WorkspaceSearchIntent {
    Find,
    Replace(String),
}

#[derive(Clone, Debug)]
enum PickerPayload {
    WorkspaceSearch(WorkspaceSearchMatch),
    LspLocation(crate::languages::LspLocation),
    Diagnostic { uri: String, range: ByteRange },
    CodeAction(crate::languages::LspCodeAction),
    Buffer(crate::editing::BufferId),
    Command(String),
    ProposalFile(usize),
    WorkspaceRoot(crate::workspace::RootId),
    LspSession(String),
    DapControl(DapControl),
    PersistedLocation(PersistedLocation),
    MatcherSource(MatcherSource),
    DapFrame(DapStackFrame),
    RegistrySummary(RegistryPackageSummary),
    RegistryMetadata(RegistryPackageMetadata),
}

#[derive(Clone, Debug)]
enum PendingVfs {
    Open,
    SessionRestore {
        requested_path: PathBuf,
    },
    WorkspaceProposalRead {
        proposal: crate::workspace::ProposalId,
        requested_path: PathBuf,
    },
    WorkspaceResourceResolve {
        proposal: crate::workspace::ProposalId,
    },
    WorkspaceProposalCreate {
        proposal: crate::workspace::ProposalId,
        requested_path: PathBuf,
    },
    LspNavigation {
        range: crate::languages::LspRange,
        encoding: PositionEncoding,
    },
    PersistedNavigation {
        location: PersistedLocation,
    },
    MatcherNavigation {
        source: MatcherSource,
    },
    LinterRead {
        generation: u64,
        raw: Vec<RawLinterDiagnostic>,
    },
    WorkspaceSearchNavigation {
        range: std::ops::Range<usize>,
    },
    ExplorerOpen {
        split: bool,
    },
    ExplorerPreview {
        path: PathBuf,
        generation: u64,
    },
    StartupOpen {
        primary: bool,
    },
    Reload {
        buffer: crate::editing::BufferId,
    },
    Save {
        buffer: crate::editing::BufferId,
        revision: BufferRevision,
        bytes: Vec<u8>,
        target: PathBuf,
        quit_after: bool,
    },
    Completion {
        generation: u64,
        prompt_input: String,
        fragment: String,
    },
    RecoveryDiscovery,
    RecoveryCleanup,
    RecoveryWrite {
        buffer: crate::editing::BufferId,
        revision: BufferRevision,
    },
    CrashDelete,
    RecoveryDelete {
        path: PathBuf,
        buffer: Option<(crate::editing::BufferId, BufferRevision)>,
    },
}

fn resolve_configuration(
    commands: &CommandRegistry<AppEffect>,
    loaded: LoadedConfig,
    strict: bool,
) -> Result<(ResolvedConfiguration, Option<String>), String> {
    let mut diagnostics = Vec::new();
    diagnostics.extend(loaded.diagnostic.iter().cloned());
    diagnostics.extend(loaded.keymaps.diagnostic.iter().cloned());
    if let Err(error) = validate_configured_commands(commands, &loaded) {
        diagnostics.push(error);
    }
    if strict && !diagnostics.is_empty() {
        return Err(diagnostics.join("; "));
    }
    let (keymaps, keymap_diagnostic) = if strict {
        match compile_keymap_entries(
            commands,
            &loaded.keymaps.entries,
            loaded.keymaps.bundled_count,
            &loaded.keymaps.path,
            &loaded.keymaps.user_spans,
        ) {
            Ok(compiled) => (compiled, None),
            Err(errors) => return Err(errors.join("; ")),
        }
    } else {
        compile_keymaps(commands, &loaded.keymaps)
    };
    diagnostics.extend(keymap_diagnostic);
    let mut settings = loaded.config;
    for alias in &settings.input.disabled_command_aliases {
        settings.input.command_aliases.remove(alias);
    }
    let keymap_source = if loaded.keymaps.entries.len() == loaded.keymaps.bundled_count {
        PathBuf::from("<bundled:keymaps>")
    } else {
        loaded.keymaps.path.clone()
    };
    let resolved = ResolvedConfiguration {
        settings,
        dashboard: loaded.dashboard,
        theme: loaded.theme,
        keymaps,
        sources: ConfigurationSources {
            settings: loaded.path,
            keymaps: keymap_source,
            dashboard: loaded.dashboard_path,
            theme: loaded.theme_path,
        },
        user_setting_keys: loaded.user_setting_keys,
        user_setting_spans: loaded.user_setting_spans,
        dashboard_spans: loaded.dashboard_spans,
        theme_spans: loaded.theme_spans,
    };
    Ok((
        resolved,
        (!diagnostics.is_empty()).then(|| diagnostics.join("; ")),
    ))
}

pub fn validate_loaded_configuration(loaded: LoadedConfig) -> Result<(), String> {
    let mut commands = CommandRegistry::default();
    register_builtin_commands(&mut commands)
        .map_err(|error| format!("built-in command registry failed: {error}"))?;
    register_app_commands(&mut commands)
        .map_err(|error| format!("application command registry failed: {error}"))?;
    resolve_configuration(&commands, loaded, true).map(|_| ())
}

fn dashboard_actions(configuration: &ResolvedConfiguration) -> Vec<DashboardAction> {
    configuration
        .dashboard
        .action
        .iter()
        .filter(|action| action.visible)
        .map(|action| DashboardAction {
            command: action.command.clone(),
            label: action.label.clone(),
            key: action
                .show_binding
                .then(|| {
                    configuration
                        .keymaps
                        .binding_for_action("dashboard", &action.command, &action.arguments)
                        .map(str::to_owned)
                })
                .flatten(),
        })
        .collect()
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
                let bundled = crate::configuration::load_bundled_config();
                let (resolved, _) = resolve_configuration(&commands, bundled, true)
                    .map_err(InteractiveError::Configuration)?;
                let message = resolved.settings.messages["startup_fallback"]
                    .replace("{diagnostic}", &diagnostic);
                (resolved, Some(message))
            }
        };
    let resolved_dashboard_actions = dashboard_actions(&configuration);
    state.policy = crate::editing::EditingPolicy {
        tab_width: configuration.settings.editor.tab_width,
        indent_width: configuration.settings.editor.indent_width,
        expand_tabs: configuration.settings.editor.expand_tabs,
        undo_break_on_text_commit: configuration.settings.editor.undo_break_on_text_commit,
        registers_enabled: configuration.settings.editor.registers.storage
            == crate::configuration::RegisterStorage::Internal,
        mirror_named_to_unnamed: configuration
            .settings
            .editor
            .registers
            .mirror_named_to_unnamed,
        ambiguous_width_wide: configuration.settings.editor.ambiguous_width
            == crate::configuration::AmbiguousWidth::Wide,
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
    let persistence_path = crate::configuration::user_state_directory()
        .join(&configuration.settings.persistence.state_file);
    let trust_state_path =
        crate::configuration::user_state_directory().join("trusted-workspaces.toml");
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
    let registry = RegistryService::start(
        configuration.settings.packages.registry.clone(),
        crate::configuration::user_state_directory().join("packages/registry-cache"),
    )
    .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
    let pty_queue_capacity = configuration.settings.terminals.queue_capacity;
    let mut application = InteractiveApplication {
        state,
        workspace,
        diagnostics,
        picker: None,
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
        configuration_worker: crate::configuration::ConfigurationWorker::start(
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
        tool_installer: ToolInstaller::start(),
        lsp: LspService::new(),
        active_lsp_server: None,
        lsp_sessions: HashMap::new(),
        lsp_session_synced: HashMap::new(),
        lsp_synced_revisions: HashMap::new(),
        lsp_capabilities: None,
        lsp_request_generation: 0,
        lsp_logs: VecDeque::new(),
        lsp_completion_items: Vec::new(),
        snippet_session: None,
        lsp_selection_ranges: Vec::new(),
        lsp_semantic_raw: HashMap::new(),
        lsp_pull_ids: HashMap::new(),
        lsp_projection_owner: None,
        lsp_request_owner: None,
        dap: DapService::new(),
        dap_responses: VecDeque::new(),
        dap_console: VecDeque::new(),
        dap_breakpoints: BTreeMap::new(),
        dap_watches: Vec::new(),
        active_debug_configuration: None,
        active_debug_terminal_profile: None,
        dap_terminal_responses: HashMap::new(),
        recovery_directory: crate::configuration::user_state_directory().join("recovery"),
        recovery_records: Vec::new(),
        recovery_revisions: HashMap::new(),
        recovery_pruned_revisions: HashMap::new(),
        recovery_scheduled: HashMap::new(),
        recovery_discovery_queued: false,
        recovery_cleanup_queued: false,
        terminal_capabilities: "pending".into(),
        next_transaction: 1_000_000,
        workspace_search: WorkspaceSearchService::default(),
        workspace_search_generation: None,
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
            crate::configuration::user_state_directory().join("live-health.json"),
        ),
        next_crash_snapshot: Instant::now(),
        plugins: PluginManager::start(plugin_queue_capacity),
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
            crate::configuration::user_state_directory().join("packages"),
            8,
            16 * 1024 * 1024,
            64 * 1024 * 1024,
        ),
        ptys: PtyService::start(pty_queue_capacity),
        terminals: HashMap::new(),
        active_terminal: None,
        execution_preflight: ExecutionPreflightService::start(process_event_capacity),
        pending_execution_preflight: HashMap::new(),
    };
    let crash_path = crate::configuration::user_state_directory().join("last-crash.json");
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
        application.process_workspace_search_events();
        application.process_explorer_result();
        application.process_trust_events();
        application.process_configuration_result();
        application.process_tool_results();
        application.process_lsp_events();
        application.restart_lsp_if_due();
        application.process_dap_events();
        application.process_execution_preflight();
        application.process_pty_events();
        application.process_plugin_events();
        application.process_registry_events();
        application.process_package_events();
        application.process_job_events();
        application.process_large_file_modes();
        application.process_syntax();
        application.process_persistence_events();
        application.sync_lsp_document();
        application.sync_lsp_status_ui();
        application.flush_keymap_timeout();
        application.update_notification_timer();
        application.update_live_crash_snapshot();
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

impl InteractiveApplication {
    fn queue_startup_reads(&mut self) {
        while let Some((path, primary)) = self.startup_reads.front().cloned() {
            match self.vfs.read(path) {
                Ok(id) => {
                    self.startup_reads.pop_front();
                    self.startup_reads_pending = self.startup_reads_pending.saturating_add(1);
                    self.pending_vfs
                        .insert(id, PendingVfs::StartupOpen { primary });
                }
                Err(VfsError::QueueFull) => break,
                Err(error) => {
                    self.startup_reads.pop_front();
                    self.show_error(error);
                }
            }
        }
        self.finish_startup_reads_if_complete();
    }

    fn finish_startup_reads_if_complete(&mut self) {
        if self.startup_reads.is_empty() && self.startup_reads_pending == 0 {
            if let Some(buffer) = self.startup_primary_buffer.take() {
                let _ = self.state.show_buffer(buffer);
                self.ui.dashboard = false;
            }
        }
    }

    fn open_tool_menu(&mut self, category: String) {
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.ui.prompt = None;
        self.rebuild_tool_menu(&category, None);
    }

    fn rebuild_tool_menu(&mut self, category: &str, selected_id: Option<&str>) {
        let configured = &self.configuration.settings.ui.tool_manager;
        let category = if configured.categories.contains_key(category) {
            category.to_owned()
        } else {
            "all".into()
        };
        let mut entries = self
            .tools
            .iter()
            .filter(|entry| category == "all" || tool_kind_key(entry.definition.kind) == category)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| {
            (
                !matches!(entry.status, ToolStatus::Installed(_)),
                entry.definition.name.to_lowercase(),
            )
        });
        let rows = entries
            .into_iter()
            .map(|entry| {
                let (status, installed) = match &entry.status {
                    ToolStatus::Installed(_) => (configured.installed_label.clone(), true),
                    ToolStatus::Available if entry.definition.install_program.is_some() => {
                        (configured.available_label.clone(), false)
                    }
                    ToolStatus::Available => {
                        (configured.unavailable_installer_label.clone(), false)
                    }
                    ToolStatus::Installing => (configured.installing_label.clone(), false),
                    ToolStatus::Failed(diagnostic) => (diagnostic.clone(), false),
                };
                ToolMenuRow {
                    id: entry.definition.id.clone(),
                    name: entry.definition.name.clone(),
                    kind: configured
                        .categories
                        .get(tool_kind_key(entry.definition.kind))
                        .cloned()
                        .unwrap_or_else(|| entry.definition.kind.label().into()),
                    languages: entry
                        .definition
                        .languages
                        .join(&configured.language_separator),
                    status,
                    installed,
                    can_install: entry.definition.install_program.is_some(),
                }
            })
            .collect::<Vec<_>>();
        let selected = selected_id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1));
        self.ui.tool_menu = Some(ToolMenuState {
            category,
            selected,
            scroll: 0,
            rows,
        });
    }

    fn move_tool_menu(&mut self, direction: i8) {
        let Some(menu) = &mut self.ui.tool_menu else {
            return;
        };
        if menu.rows.is_empty() {
            return;
        }
        menu.selected = if direction < 0 {
            menu.selected.saturating_sub(1)
        } else {
            menu.selected.saturating_add(1).min(menu.rows.len() - 1)
        };
        let visible = usize::from(self.terminal_height.saturating_sub(12)).max(1);
        if menu.selected < menu.scroll {
            menu.scroll = menu.selected;
        } else if menu.selected >= menu.scroll.saturating_add(visible) {
            menu.scroll = menu.selected.saturating_add(1).saturating_sub(visible);
        }
    }

    fn cycle_tool_category(&mut self, direction: i8) {
        let Some(menu) = &self.ui.tool_menu else {
            return;
        };
        let order = &self.configuration.settings.ui.tool_manager.category_order;
        let current = order
            .iter()
            .position(|category| category == &menu.category)
            .unwrap_or(0);
        let next = if direction < 0 {
            if current == 0 {
                order.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % order.len()
        };
        let category = order[next].clone();
        self.rebuild_tool_menu(&category, None);
    }

    fn refresh_tools(&mut self) {
        let category = self
            .ui
            .tool_menu
            .as_ref()
            .map_or_else(|| "all".into(), |menu| menu.category.clone());
        let selected = self
            .ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.rows.get(menu.selected))
            .map(|row| row.id.clone());
        self.tools = inspect_tools();
        self.rebuild_tool_menu(&category, selected.as_deref());
    }

    fn activate_selected_tool(&mut self) {
        let selected = self
            .ui
            .tool_menu
            .as_ref()
            .and_then(|menu| menu.rows.get(menu.selected))
            .map(|row| row.id.clone());
        let Some(id) = selected else {
            return;
        };
        self.activate_tool_by_id(&id);
    }

    fn activate_tool_by_id(&mut self, id: &str) {
        let Some(index) = self
            .tools
            .iter()
            .position(|entry| entry.definition.id == id)
        else {
            return;
        };
        if matches!(self.tools[index].status, ToolStatus::Installed(_)) {
            match self.tools[index].definition.kind {
                ToolKind::Lsp => self.start_lsp(Some(id.into())),
                ToolKind::Dap => self.start_dap(Some(id.into()), None),
                ToolKind::Formatter | ToolKind::Linter => {
                    self.ui.notification = Some(self.message(
                        "tool_install_succeeded",
                        &[("tool", self.tools[index].definition.name.clone())],
                    ));
                    self.ui.notification_error = false;
                }
            }
            return;
        }
        let definition = self.tools[index].definition.clone();
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::InstallTool(id.into()));
            return;
        }
        if definition.install_program.is_none() {
            self.show_error(self.message("tool_no_installer", &[("tool", definition.name)]));
            return;
        }
        let program = definition
            .install_program
            .as_ref()
            .and_then(|program| discover_executable(program));
        let Some(program) = program else {
            self.show_error(self.message(
                "tool_install_failed",
                &[
                    ("tool", definition.name),
                    ("diagnostic", "installer executable is unavailable".into()),
                ],
            ));
            return;
        };
        let Some(root_id) = self.workspace.active_root() else {
            return;
        };
        let Some(cwd) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let request = ExecutionPreflightRequest {
            executable: program,
            cwd,
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(preflight) => {
                self.pending_execution_preflight.insert(
                    preflight,
                    PendingExecutionPreflight::ToolInstall {
                        definition: definition.clone(),
                    },
                );
                self.tools[index].status = ToolStatus::Installing;
                self.ui.notification =
                    Some(self.message("tool_install_started", &[("tool", definition.name)]));
                self.ui.notification_error = false;
                let category = self
                    .ui
                    .tool_menu
                    .as_ref()
                    .map_or("all", |menu| &menu.category)
                    .to_owned();
                self.rebuild_tool_menu(&category, Some(id));
            }
            Err(error) => self.show_error(error),
        }
    }

    fn process_tool_results(&mut self) {
        while let Ok(Some(result)) = self.tool_installer.try_result() {
            let Some(index) = self
                .tools
                .iter()
                .position(|entry| entry.definition.id == result.id)
            else {
                continue;
            };
            let name = self.tools[index].definition.name.clone();
            self.tools[index].status = match result.result {
                Ok(Some(path)) => {
                    self.ui.notification =
                        Some(self.message("tool_install_succeeded", &[("tool", name)]));
                    self.ui.notification_error = false;
                    ToolStatus::Installed(path)
                }
                Ok(None) => {
                    let diagnostic = "installer succeeded but executable is not on PATH".to_owned();
                    self.show_error(self.message(
                        "tool_install_failed",
                        &[("tool", name), ("diagnostic", diagnostic.clone())],
                    ));
                    ToolStatus::Failed(diagnostic)
                }
                Err(diagnostic) => {
                    self.show_error(self.message(
                        "tool_install_failed",
                        &[("tool", name), ("diagnostic", diagnostic.clone())],
                    ));
                    ToolStatus::Failed(diagnostic)
                }
            };
            if let Some(menu) = &self.ui.tool_menu {
                let category = menu.category.clone();
                self.rebuild_tool_menu(&category, Some(&result.id));
            }
        }
    }

    fn active_lsp_document(&self) -> Option<(crate::editing::BufferId, LspDocument)> {
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.clone()?;
        let language_id = language_id_for_path(&path)?.to_owned();
        Some((
            buffer_id,
            LspDocument {
                path,
                language_id,
                version: buffer.text.revision().0,
                text: buffer.text.to_string(),
            },
        ))
    }

    fn start_lsp(&mut self, requested: Option<String>) {
        if !self.configuration.settings.lsp.enabled {
            self.show_error(self.message("lsp_disabled", &[]));
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let entry = requested
            .as_ref()
            .and_then(|id| {
                self.tools.iter().find(|entry| {
                    entry.definition.id == *id
                        && entry.definition.kind == ToolKind::Lsp
                        && entry.definition.languages.contains(&document.language_id)
                })
            })
            .or_else(|| {
                self.tools.iter().find(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry.definition.languages.contains(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
            });
        let Some(entry) = entry else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        let ToolStatus::Installed(executable) = &entry.status else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        let definition = entry.definition.clone();
        let executable = executable.clone();
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let session_key = crate::languages::LspSessionKey {
            root_id: root_id.0,
            server_id: definition.id.clone(),
        }
        .transport_key();
        let running = self.lsp.running_servers().contains(&session_key);
        let starting=self.pending_execution_preflight.values().any(|pending|matches!(pending,PendingExecutionPreflight::Lsp{session_key:key,..}if key==&session_key));
        if running || starting {
            self.active_lsp_server = Some(session_key);
            return;
        }
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::StartLsp(Some(definition.id.clone())));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::SystemTool)
        {
            self.show_error(error);
            return;
        }
        let restart = self.lsp_sessions.contains_key(&session_key);
        self.queue_lsp_preflight(
            session_key,
            definition,
            executable,
            root_id,
            root_path,
            buffer,
            document,
            restart,
        );
    }

    fn queue_lsp_preflight(
        &mut self,
        session_key: String,
        definition: ToolDefinition,
        executable: PathBuf,
        root_id: crate::workspace::RootId,
        root_path: PathBuf,
        buffer: crate::editing::BufferId,
        document: LspDocument,
        restart: bool,
    ) {
        let request = ExecutionPreflightRequest {
            executable,
            cwd: root_path,
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Lsp {
                        session_key,
                        definition,
                        root_id,
                        buffer,
                        document,
                        restart,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    fn launch_preflighted_lsp(
        &mut self,
        session_key: String,
        definition: ToolDefinition,
        root_id: crate::workspace::RootId,
        buffer: crate::editing::BufferId,
        mut document: LspDocument,
        result: crate::processes::ExecutionPreflightResult,
        restart: bool,
    ) {
        if result.root_revision != self.workspace.root_revision() {
            return;
        }
        let Some(current) = self.state.buffers.get(buffer) else {
            return;
        };
        if current.path.as_ref() != Some(&document.path)
            || !language_id_for_path(&document.path)
                .is_some_and(|language| language == document.language_id)
            || !definition.languages.contains(&document.language_id)
            || !self
                .workspace
                .require_path(&document.path)
                .is_ok_and(|owner| owner == root_id)
        {
            return;
        }
        document.version = current.text.revision().0;
        document.text = current.text.to_string();
        let policy = crate::languages::LspPolicy {
            request_timeout: Duration::from_millis(
                self.configuration.settings.lsp.request_timeout_ms,
            ),
            maximum_log_lines: self.configuration.settings.lsp.maximum_log_lines,
        };
        let server_name = definition.name.clone();
        match self.lsp.start_session_absolute_with_policy(
            &session_key,
            &definition,
            result.executable.clone(),
            result.cwd.clone(),
            Some(document.clone()),
            policy,
        ) {
            Ok(()) => {
                let mut documents = self
                    .lsp_sessions
                    .get(&session_key)
                    .map_or_else(HashMap::new, |state| state.documents.clone());
                documents.insert(buffer, document.clone());
                if restart {
                    for (replay_buffer, replay) in &documents {
                        if *replay_buffer != buffer {
                            let _ = self.lsp.sync(&session_key, replay.clone());
                        }
                    }
                }
                let restart_count = self
                    .lsp_sessions
                    .get(&session_key)
                    .map_or(0, |state| state.restart_count);
                let activate = self
                    .lsp_sessions
                    .get(&session_key)
                    .map_or(true, |state| state.activate_on_restart);
                self.lsp_sessions.insert(
                    session_key.clone(),
                    LspRuntimeState {
                        root: root_id,
                        root_path: result.cwd,
                        server_id: definition.id.clone(),
                        definition,
                        executable: result.executable,
                        capabilities: None,
                        documents,
                        restart_count,
                        restart_due: None,
                        circuit_open: false,
                        desired_running: true,
                        activate_on_restart: activate,
                    },
                );
                if activate {
                    self.active_lsp_server = Some(session_key.clone());
                    self.lsp_capabilities = None;
                }
                self.lsp_session_synced
                    .insert((session_key, buffer), BufferRevision(document.version));
                self.ui.notification =
                    Some(self.message("lsp_started", &[("server", server_name)]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn restart_lsp_session(&mut self, key: String) {
        let Some(state) = self.lsp_sessions.get(&key) else {
            return;
        };
        if state.circuit_open || !state.desired_running {
            return;
        }
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::RestartLsp(key));
            return;
        }
        let Some((&buffer, document)) = state.documents.iter().next() else {
            return;
        };
        let (definition, executable, root, root_path, document) = (
            state.definition.clone(),
            state.executable.clone(),
            state.root,
            state.root_path.clone(),
            document.clone(),
        );
        self.queue_lsp_preflight(
            key, definition, executable, root, root_path, buffer, document, true,
        );
    }

    fn select_lsp_session(&mut self, key: &str) {
        for state in self.lsp_sessions.values_mut() {
            state.activate_on_restart = false
        }
        if let Some(session) = self.lsp_sessions.get_mut(key) {
            session.activate_on_restart = true;
            self.active_lsp_server = Some(key.into());
            self.lsp_capabilities = session.capabilities.clone();
        }
    }
    fn show_lsp_sessions(&mut self) {
        let active = self.active_lsp_server.as_deref();
        let items = self
            .lsp_sessions
            .iter()
            .map(|(key, session)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("lsp-session:{key}")),
                        label: session.server_id.clone(),
                        detail: format!(
                            "root #{}{} · {} · restarts {}",
                            session.root.0,
                            if active == Some(key) {
                                " · active"
                            } else {
                                ""
                            },
                            if session.circuit_open {
                                "circuit-open"
                            } else if session.restart_due.is_some() {
                                "backoff"
                            } else if session.desired_running {
                                "running"
                            } else {
                                "stopped"
                            },
                            session.restart_count
                        ),
                        source: key.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspSession(key.clone()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_lsp_sessions_title", &[]), items);
    }

    fn clear_lsp_server_ownership(&mut self, server: &str) {
        self.lsp_session_synced
            .retain(|(candidate, _), _| candidate != server);
        self.lsp_semantic_raw
            .retain(|(candidate, _), _| candidate != server);
        self.lsp_pull_ids
            .retain(|(candidate, _), _| candidate != server);
        for prefix in ["lsp.", "lsp-pull:"] {
            if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
    }
    fn detach_lsp_documents(
        &mut self,
        buffer: crate::editing::BufferId,
        documents: Vec<(String, String)>,
    ) {
        let detached = !documents.is_empty();
        for (server, uri) in documents {
            let _ = self.lsp.close(&server, uri.clone());
            self.lsp_session_synced.remove(&(server.clone(), buffer));
            self.lsp_semantic_raw.remove(&(server.clone(), uri.clone()));
            self.lsp_pull_ids.remove(&(server.clone(), uri.clone()));
            for prefix in ["lsp.", "lsp-pull:"] {
                if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                    self.diagnostics.clear_owner_uri(&owner, &uri);
                }
            }
            if let Some(state) = self.lsp_sessions.get_mut(&server) {
                state.documents.remove(&buffer);
            }
        }
        if detached {
            self.sync_ui_diagnostics();
            if self.state.active_buffer_id() == Some(buffer) {
                self.clear_lsp_ui_projection();
            }
        }
    }
    fn sync_lsp_status_ui(&mut self) {
        self.ui.lsp_server = self
            .active_lsp_server
            .as_ref()
            .and_then(|server| self.lsp_sessions.get(server))
            .map(|session| session.server_id.clone());
    }
    fn sync_lsp_document(&mut self) {
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        let Some((_, document)) = self.active_lsp_document() else {
            let stale = self
                .lsp_sessions
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .documents
                        .get(&buffer)
                        .map(|document| (key.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer, stale);
            self.clear_lsp_ui_projection();
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            return;
        };
        let revision = BufferRevision(document.version);
        let mut sessions = Vec::new();
        let mut detach = Vec::new();
        for (key, state) in &self.lsp_sessions {
            let owns = self
                .workspace
                .roots()
                .iter()
                .find(|root| root.id == state.root)
                .is_some_and(|root| document.path.starts_with(&root.identity.canonical_path));
            let supports = state.definition.languages.contains(&document.language_id);
            if owns && supports {
                sessions.push(key.clone());
                if let Some(previous) = state.documents.get(&buffer) {
                    if previous.path != document.path
                        || previous.language_id != document.language_id
                    {
                        detach.push((key.clone(), previous.uri()));
                    }
                }
            } else if let Some(previous) = state.documents.get(&buffer) {
                detach.push((key.clone(), previous.uri()));
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            self.clear_lsp_ui_projection();
        }
        if !sessions.is_empty()
            && !sessions
                .iter()
                .any(|key| self.active_lsp_server.as_ref() == Some(key))
        {
            self.select_lsp_session(&sessions[0]);
        }
        self.detach_lsp_documents(buffer, detach);
        self.refresh_lsp_projection_owner();
        for server in &sessions {
            if self.lsp_session_synced.get(&(server.clone(), buffer)) == Some(&revision) {
                continue;
            }
            if self.lsp.change(server, document.clone()).is_ok() {
                self.lsp_session_synced
                    .insert((server.clone(), buffer), revision);
                if let Some(state) = self.lsp_sessions.get_mut(server) {
                    state.documents.insert(buffer, document.clone());
                }
            }
        }
        if self.ui.language_completion.is_some()
            && self
                .active_lsp_server
                .as_ref()
                .is_some_and(|active| sessions.contains(active))
        {
            self.request_lsp(LspFeature::Completion);
        }
    }

    fn process_lsp_events(&mut self) {
        while let Some(event) = self.lsp.try_event() {
            match event {
                LspEvent::Starting { .. } => {}
                LspEvent::Running {
                    server,
                    capabilities,
                } => {
                    if let Some(session) = self.lsp_sessions.get_mut(&server) {
                        session.capabilities = Some(capabilities.clone());
                    }
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.lsp_capabilities = Some(capabilities);
                    }
                    self.ui.notification = Some(self.message("lsp_running", &[("server", server)]));
                    self.ui.notification_error = false;
                }
                LspEvent::CapabilitiesChanged {
                    server,
                    capabilities,
                } => {
                    if let Some(session) = self.lsp_sessions.get_mut(&server) {
                        session.capabilities = Some(capabilities.clone());
                    }
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.lsp_capabilities = Some(capabilities);
                    }
                }
                LspEvent::Diagnostics {
                    server,
                    uri,
                    version,
                    diagnostics,
                } => {
                    let current = self.state.buffers.iter().find_map(|(_, buffer)| {
                        buffer.path.as_ref().and_then(|path| {
                            (crate::languages::file_uri(path) == uri)
                                .then_some(buffer.text.revision())
                        })
                    });
                    let normalized_version = version.map(BufferRevision);
                    let encoding = self
                        .lsp_capabilities
                        .as_ref()
                        .map_or(PositionEncoding::Utf16, |capabilities| {
                            capabilities.position_encoding
                        });
                    let normalized = self
                        .state
                        .buffers
                        .iter()
                        .find_map(|(_, buffer)| {
                            let path = buffer.path.as_ref()?;
                            (crate::languages::file_uri(path) == uri).then(|| {
                                let codec = PositionCodec::new(&buffer.text);
                                diagnostics
                                    .iter()
                                    .filter_map(|diagnostic| {
                                        let start = codec
                                            .lsp_to_byte(
                                                diagnostic.range.start.line,
                                                diagnostic.range.start.character,
                                                encoding,
                                            )
                                            .ok()?;
                                        let end = codec
                                            .lsp_to_byte(
                                                diagnostic.range.end.line,
                                                diagnostic.range.end.character,
                                                encoding,
                                            )
                                            .ok()?;
                                        Some(Diagnostic {
                                            uri: uri.clone(),
                                            version: normalized_version,
                                            range: ByteRange::new(start, end).ok()?,
                                            severity: match diagnostic.severity {
                                                Some(1) => DiagnosticSeverity::Error,
                                                Some(2) => DiagnosticSeverity::Warning,
                                                Some(3) => DiagnosticSeverity::Information,
                                                _ => DiagnosticSeverity::Hint,
                                            },
                                            source: diagnostic
                                                .source
                                                .clone()
                                                .unwrap_or_else(|| server.clone()),
                                            code: diagnostic.code.clone(),
                                            message: diagnostic.message.clone(),
                                            related: Vec::new(),
                                            tags: Vec::new(),
                                            fixes: Vec::new(),
                                            timestamp: crate::clock::system_time(),
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .unwrap_or_default();
                    let count = normalized.len();
                    let owner = DiagnosticOwner::new(format!("lsp.{server}"));
                    match owner.and_then(|owner| {
                        self.diagnostics.replace(
                            owner,
                            uri,
                            normalized_version,
                            current,
                            normalized,
                        )
                    }) {
                        Ok(()) => {
                            self.sync_ui_diagnostics();
                            self.ui.notification = Some(self.message(
                                "lsp_diagnostics",
                                &[("server", server), ("count", count.to_string())],
                            ));
                            self.ui.notification_error = false;
                        }
                        Err(error) => {
                            self.lsp_logs.push_back(error.to_string());
                        }
                    }
                }
                LspEvent::Result {
                    server,
                    generation,
                    method,
                    uri,
                    version,
                    result,
                } => {
                    let current = self.active_lsp_document();
                    if generation != self.lsp_request_generation
                        || !current.as_ref().is_some_and(|(_, document)| {
                            document.uri() == uri && document.version == version
                        })
                    {
                        continue;
                    }
                    self.refresh_lsp_projection_owner();
                    if !self
                        .lsp_request_owner
                        .as_ref()
                        .is_some_and(|(owned_generation, owner)| {
                            *owned_generation == generation
                                && Some(owner) == self.lsp_projection_owner.as_ref()
                                && owner.server == server
                                && owner.uri == uri
                                && owner.revision.0 == version
                        })
                    {
                        continue;
                    }
                    self.lsp_request_owner = None;
                    self.present_lsp_result(&server, &method, &uri, result);
                }
                LspEvent::ApplyEditRequest {
                    server,
                    request_id,
                    label,
                    edit,
                } => {
                    self.begin_workspace_proposal(
                        label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                        edit,
                        Some((server, request_id)),
                        self.lsp_position_encoding(),
                    );
                }
                LspEvent::Progress {
                    server,
                    title,
                    message,
                } => {
                    self.ui.notification = Some(self.message(
                        "lsp_progress",
                        &[("server", server), ("title", title), ("progress", message)],
                    ));
                    self.ui.notification_error = false;
                }
                LspEvent::Log { server, message } => {
                    if self.lsp_logs.len() == 200 {
                        self.lsp_logs.pop_front();
                    }
                    self.lsp_logs.push_back(format!("{server}: {message}"));
                }
                LspEvent::Health { server, health } => {
                    self.ui.notification = Some(self.message(
                        "lsp_health",
                        &[
                            ("server", server),
                            ("documents", health.attached_documents.to_string()),
                            ("pending", health.pending_requests.to_string()),
                            (
                                "encoding",
                                health.capabilities.position_encoding.lsp_name().into(),
                            ),
                        ],
                    ));
                    self.ui.notification_error = false;
                }
                LspEvent::Unsupported { server, method } => self.show_error(
                    self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                ),
                LspEvent::RequestFailed {
                    server,
                    method,
                    message,
                    ..
                } => self.show_error(self.message(
                    "lsp_request_failed",
                    &[
                        ("server", server),
                        ("method", method),
                        ("diagnostic", message),
                    ],
                )),
                LspEvent::Stopped { server, .. } => {
                    let desired = self
                        .lsp_sessions
                        .get(&server)
                        .is_some_and(|state| state.desired_running);
                    let _ = self.lsp.stop(&server);
                    self.lsp_session_synced.retain(|(key, _), _| key != &server);
                    self.ui.notification =
                        Some(self.message("lsp_stopped", &[("server", server.clone())]));
                    self.ui.notification_error = false;
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
                LspEvent::Failed { server, message } => {
                    let desired = self
                        .lsp_sessions
                        .get(&server)
                        .is_some_and(|state| state.desired_running);
                    let _ = self.lsp.stop(&server);
                    self.lsp_session_synced.retain(|(key, _), _| key != &server);
                    self.show_error(self.message(
                        "lsp_failed",
                        &[("server", server.clone()), ("diagnostic", message)],
                    ));
                    if self.active_lsp_server.as_deref() == Some(&server) {
                        self.clear_lsp_session_state();
                    }
                    if desired {
                        self.schedule_lsp_restart(server);
                    }
                }
            }
        }
    }

    fn schedule_lsp_restart(&mut self, key: String) {
        let limit = self.configuration.settings.lsp.restart_limit;
        let Some(state) = self.lsp_sessions.get_mut(&key) else {
            return;
        };
        if !state.desired_running || state.restart_due.is_some() {
            return;
        }
        if state.restart_count >= limit {
            state.restart_due = None;
            state.circuit_open = true;
            let server = state.server_id.clone();
            let _ = state;
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
            return;
        }
        state.restart_count = state.restart_count.saturating_add(1);
        let exponent = u32::from(state.restart_count.saturating_sub(1)).min(8);
        let delay = self
            .configuration
            .settings
            .lsp
            .restart_backoff_ms
            .saturating_mul(2_u64.saturating_pow(exponent))
            .min(60_000);
        state.restart_due = Some(Instant::now() + Duration::from_millis(delay));
    }

    fn restart_lsp_if_due(&mut self) {
        if self.execution_validation.is_some() {
            return;
        }
        let now = Instant::now();
        let key = self.lsp_sessions.iter().find_map(|(key, state)| {
            (state.desired_running
                && !state.circuit_open
                && state.restart_due.is_some_and(|due| due <= now))
            .then(|| key.clone())
        });
        let Some(key) = key else { return };
        if let Some(state) = self.lsp_sessions.get_mut(&key) {
            state.restart_due = None;
        }
        self.restart_lsp_session(key);
    }

    fn sync_ui_diagnostics(&mut self) {
        self.ui.diagnostics = self
            .diagnostics
            .problems(None, None)
            .into_iter()
            .filter_map(|problem| {
                Some(UiDiagnostic {
                    path: path_from_file_uri(&problem.uri)?,
                    range: problem.range,
                    message: problem.message,
                })
            })
            .collect();
    }

    fn clear_lsp_ui_projection(&mut self) {
        self.lsp_completion_items.clear();
        self.ui.language_completion = None;
        self.ui.lsp_semantic_spans.clear();
        self.ui.lsp_highlight_spans.clear();
        self.ui.lsp_inlay_hints.clear();
        self.ui.lsp_folds.clear();
        self.lsp_selection_ranges.clear();
        self.lsp_projection_owner = None;
        self.lsp_request_owner = None;
    }
    fn current_lsp_projection_owner(&self) -> Option<LspProjectionOwner> {
        let server = self.active_lsp_server.clone()?;
        let view = self.state.active_view?;
        let (buffer, document) = self.active_lsp_document()?;
        let session = self.lsp_sessions.get(&server)?;
        if !session.definition.languages.contains(&document.language_id)
            || !document.path.starts_with(&session.root_path)
        {
            return None;
        }
        Some(LspProjectionOwner {
            server,
            buffer,
            view,
            revision: BufferRevision(document.version),
            uri: document.uri(),
            language: document.language_id,
        })
    }
    fn refresh_lsp_projection_owner(&mut self) {
        let current = self.current_lsp_projection_owner();
        if self.lsp_projection_owner != current {
            self.clear_lsp_ui_projection();
            self.lsp_projection_owner = current;
        }
    }
    fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.active_lsp_server.clone() {
            self.clear_lsp_server_ownership(&server);
        }
        self.active_lsp_server = None;
        self.lsp_synced_revisions.clear();
        self.lsp_capabilities = None;
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }

    fn request_lsp(&mut self, mut feature: LspFeature) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((_, document)) = self.active_lsp_document() else {
            return;
        };
        let supports = |state: &LspRuntimeState| {
            state.definition.languages.contains(&document.language_id)
                && document.path.starts_with(&state.root_path)
        };
        let server = self
            .active_lsp_server
            .as_ref()
            .filter(|key| self.lsp_sessions.get(*key).is_some_and(&supports))
            .cloned()
            .or_else(|| {
                let mut matches = self
                    .lsp_sessions
                    .iter()
                    .filter_map(|(key, state)| supports(state).then_some(key.clone()))
                    .collect::<Vec<_>>();
                matches.sort();
                matches.into_iter().next()
            });
        let Some(server) = server else {
            self.start_lsp(None);
            return;
        };
        if self.active_lsp_server.as_ref() != Some(&server) {
            self.select_lsp_session(&server);
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            let encoding = self
                .lsp_capabilities
                .as_ref()
                .map_or(PositionEncoding::Utf16, |capabilities| {
                    capabilities.position_encoding
                });
            let buffer = TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&buffer);
            let (start, end) = selection.map_or((offset, offset), |selection| {
                if selection.anchor <= selection.head {
                    (selection.anchor, selection.head)
                } else {
                    (selection.head, selection.anchor)
                }
            });
            let Ok((start_line, start_character)) = codec.byte_to_lsp(start, encoding) else {
                return;
            };
            let Ok((end_line, end_character)) = codec.byte_to_lsp(end, encoding) else {
                return;
            };
            feature = LspFeature::CodeAction {
                range: crate::languages::LspRange {
                    start: crate::languages::LspPosition {
                        line: start_line,
                        character: start_character,
                    },
                    end: crate::languages::LspPosition {
                        line: end_line,
                        character: end_character,
                    },
                },
            };
        }
        if matches!(feature, LspFeature::PullDiagnostics { .. })
            && let Some(previous) = self.lsp_pull_ids.get(&(server.clone(), document.uri()))
        {
            feature = LspFeature::PullDiagnostics {
                previous_result_id: Some(previous.clone()),
            };
        }
        if matches!(feature, LspFeature::SemanticTokensFull)
            && let Some((Some(previous), _)) =
                self.lsp_semantic_raw.get(&(server.clone(), document.uri()))
        {
            feature = LspFeature::SemanticTokensDelta {
                previous_result_id: previous.clone(),
            };
        }
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.lsp_projection_owner.clone() else {
            return;
        };
        self.lsp_request_generation = self.lsp_request_generation.saturating_add(1);
        let generation = self.lsp_request_generation;
        match self
            .lsp
            .request(&server, feature, document, offset, generation)
        {
            Ok(()) => self.lsp_request_owner = Some((generation, owner)),
            Err(error) => self.show_error(error),
        }
    }

    fn present_lsp_result(
        &mut self,
        server: &str,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
    ) {
        match result {
            LspFeatureResult::Hover(text) | LspFeatureResult::Signature(text) => {
                self.ui.notification = Some(self.message(
                    "lsp_hover",
                    &[
                        ("server", server.into()),
                        ("hover", text.replace('\n', " ")),
                    ],
                ));
                self.ui.notification_error = false;
            }
            LspFeatureResult::Completion(items) => {
                self.lsp_completion_items = items;
                self.ui.language_completion =
                    (!self.lsp_completion_items.is_empty()).then(|| LanguageCompletionState {
                        items: self
                            .lsp_completion_items
                            .iter()
                            .map(|item| CompletionItem {
                                label: item.label.clone(),
                                detail: item.detail.clone(),
                            })
                            .collect(),
                        selected: 0,
                    });
                if self.ui.language_completion.is_none() {
                    self.show_error(self.message("lsp_no_result", &[]));
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                if !item.documentation.is_empty() {
                    self.ui.notification = Some(item.documentation.replace('\n', " "));
                    self.ui.notification_error = false;
                }
                if let Some(selected) = self
                    .ui
                    .language_completion
                    .as_ref()
                    .map(|completion| completion.selected)
                    && selected < self.lsp_completion_items.len()
                {
                    self.lsp_completion_items[selected] = item;
                    self.accept_lsp_completion();
                }
            }
            LspFeatureResult::Locations(locations) => {
                if locations.is_empty() {
                    self.show_error(self.message("lsp_no_result", &[]));
                } else if method == "textDocument/references" || locations.len() > 1 {
                    self.open_lsp_locations_picker(server, locations);
                } else if let Some(location) = locations.first() {
                    self.navigate_lsp_location(location);
                }
            }
            LspFeatureResult::Symbols(symbols) => {
                self.open_lsp_symbols_picker(server, symbols);
            }
            LspFeatureResult::WorkspaceEdit(mut edit) => {
                for document in &mut edit.documents {
                    if document.uri.is_empty() {
                        document.uri = uri.into();
                    }
                }
                self.begin_workspace_proposal(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.open_lsp_code_actions_picker(server, actions);
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.lsp_pull_ids.insert((server.into(), uri.into()), id);
                }
                if result.unchanged {
                    return;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((_, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                    && let Ok(owner) = DiagnosticOwner::new(format!("lsp-pull:{server}"))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let diagnostics = result
                        .items
                        .into_iter()
                        .filter_map(|item| {
                            let start = codec
                                .lsp_to_byte(
                                    item.range.start.line,
                                    item.range.start.character,
                                    encoding,
                                )
                                .ok()?;
                            let end = codec
                                .lsp_to_byte(
                                    item.range.end.line,
                                    item.range.end.character,
                                    encoding,
                                )
                                .ok()?;
                            let range = ByteRange::new(start, end).ok()?;
                            Some(Diagnostic {
                                uri: uri.into(),
                                version: Some(buffer.text.revision()),
                                range,
                                severity: match item.severity.unwrap_or(1) {
                                    1 => DiagnosticSeverity::Error,
                                    2 => DiagnosticSeverity::Warning,
                                    3 => DiagnosticSeverity::Information,
                                    _ => DiagnosticSeverity::Hint,
                                },
                                source: item.source.unwrap_or_else(|| server.into()),
                                code: item.code,
                                message: item.message,
                                related: Vec::new(),
                                tags: Vec::new(),
                                fixes: Vec::new(),
                                timestamp: crate::clock::system_time(),
                            })
                        })
                        .collect();
                    let revision = Some(buffer.text.revision());
                    let _ = self.diagnostics.replace(
                        owner,
                        uri.into(),
                        revision,
                        revision,
                        diagnostics,
                    );
                    self.sync_ui_diagnostics();
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                self.lsp_semantic_raw.insert(
                    (server.into(), uri.into()),
                    (result.result_id.clone(), result.data.clone()),
                );
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let legend = self
                        .lsp_capabilities
                        .as_ref()
                        .map(|value| value.semantic_token_types.as_slice())
                        .unwrap_or(&[]);
                    let mut line = 0usize;
                    let mut character = 0usize;
                    let mut spans = Vec::new();
                    for token in result.data.chunks_exact(5).take(100_000) {
                        line = line.saturating_add(token[0] as usize);
                        character = if token[0] == 0 {
                            character.saturating_add(token[1] as usize)
                        } else {
                            token[1] as usize
                        };
                        let Some(name) = legend.get(token[3] as usize) else {
                            continue;
                        };
                        let Ok(start) = codec.lsp_to_byte(line, character, encoding) else {
                            continue;
                        };
                        let Ok(end) = codec.lsp_to_byte(
                            line,
                            character.saturating_add(token[2] as usize),
                            encoding,
                        ) else {
                            continue;
                        };
                        let Ok(range) = ByteRange::new(start, end) else {
                            continue;
                        };
                        spans.push(UiSyntaxSpan {
                            buffer: buffer_id,
                            revision: buffer.text.revision(),
                            range,
                            role: semantic_role(name),
                        });
                    }
                    self.ui.lsp_semantic_spans = spans;
                }
            }
            LspFeatureResult::SemanticTokenDelta(delta) => {
                let key = (server.to_owned(), uri.to_owned());
                let mut valid = false;
                if let Some((result_id, data)) = self.lsp_semantic_raw.get_mut(&key) {
                    valid = true;
                    for edit in delta.edits {
                        if edit.start.saturating_add(edit.delete_count) > data.len()
                            || data
                                .len()
                                .saturating_sub(edit.delete_count)
                                .saturating_add(edit.data.len())
                                > 500_000
                        {
                            valid = false;
                            break;
                        }
                        data.splice(edit.start..edit.start + edit.delete_count, edit.data);
                    }
                    if data.len() % 5 != 0 {
                        valid = false
                    }
                    if valid {
                        *result_id = delta.result_id.clone();
                        let full = crate::languages::LspSemanticTokens {
                            result_id: result_id.clone(),
                            data: data.clone(),
                        };
                        self.present_lsp_result(
                            server,
                            method,
                            uri,
                            LspFeatureResult::SemanticTokens(full),
                        );
                    }
                }
                if !valid {
                    self.lsp_semantic_raw.remove(&key);
                    self.request_lsp(LspFeature::SemanticTokensFull);
                }
            }
            LspFeatureResult::InlayHints(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    self.ui.lsp_inlay_hints = items
                        .into_iter()
                        .filter_map(|item| {
                            Some(UiInlayHint {
                                buffer: buffer_id,
                                revision: buffer.text.revision(),
                                position: codec
                                    .lsp_to_byte(
                                        item.position.line,
                                        item.position.character,
                                        encoding,
                                    )
                                    .ok()?,
                                label: item.label,
                            })
                        })
                        .collect();
                }
            }
            LspFeatureResult::CodeLens(items) => {
                let rows = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("code-lens:{index}")),
                                label: item.title,
                                detail: item.command.unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_code_lens_title", &[]), rows);
            }
            LspFeatureResult::Ranges(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let ranges = items
                        .into_iter()
                        .filter_map(|item| {
                            ByteRange::new(
                                codec
                                    .lsp_to_byte(item.start.line, item.start.character, encoding)
                                    .ok()?,
                                codec
                                    .lsp_to_byte(item.end.line, item.end.character, encoding)
                                    .ok()?,
                            )
                            .ok()
                        })
                        .collect::<Vec<_>>();
                    match method {
                        "textDocument/documentHighlight" => {
                            self.ui.lsp_highlight_spans = ranges
                                .into_iter()
                                .map(|range| UiSyntaxSpan {
                                    buffer: buffer_id,
                                    revision: buffer.text.revision(),
                                    range,
                                    role: crate::syntax::SyntaxRole::Variable,
                                })
                                .collect()
                        }
                        "textDocument/foldingRange" => {
                            if let Some(view) = self.state.active_view {
                                self.ui.lsp_folds = ranges
                                    .into_iter()
                                    .filter_map(|range| {
                                        Some(UiFold {
                                            view,
                                            start_line: buffer
                                                .text
                                                .byte_to_line(range.start)
                                                .ok()?,
                                            end_line: buffer.text.byte_to_line(range.end).ok()?,
                                            collapsed: false,
                                        })
                                    })
                                    .collect()
                            }
                        }
                        "textDocument/selectionRange" => self.lsp_selection_ranges = ranges,
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Empty => self.show_error(self.message("lsp_no_result", &[])),
        }
    }

    fn sync_explorer_ui(&mut self) {
        if self.ui.explorer.is_none() {
            return;
        }
        let hide_single_root = self.workspace.roots().len() == 1;
        let visible = self
            .explorer
            .visible()
            .into_iter()
            .filter(|node| !(hide_single_root && node.kind == ExplorerKind::Root))
            .collect::<Vec<_>>();
        let selected = self
            .explorer
            .selected_id()
            .and_then(|id| visible.iter().position(|node| node.id == id));
        let mut rows: Vec<PickerRow> = visible
            .into_iter()
            .map(|node| {
                let marker = match (node.kind, node.load, node.expanded) {
                    (
                        ExplorerKind::Root | ExplorerKind::Directory,
                        ExplorerLoadState::Loading(_),
                        _,
                    ) => self.message("explorer_loading_marker", &[]),
                    (ExplorerKind::Root | ExplorerKind::Directory, _, true) => {
                        self.message("explorer_expanded_marker", &[])
                    }
                    (ExplorerKind::Root | ExplorerKind::Directory, _, false) => {
                        self.message("explorer_collapsed_marker", &[])
                    }
                    (ExplorerKind::File, _, _) => self.message("explorer_file_marker", &[]),
                    (ExplorerKind::Symlink, _, _) => self.message("explorer_symlink_marker", &[]),
                };
                PickerRow {
                    id: node.id.0.to_string(),
                    label: self.message(
                        "explorer_row",
                        &[
                            (
                                "indent",
                                self.explorer.tree_prefix(
                                    node.id,
                                    self.configuration.settings.ui.explorer.indent_width,
                                ),
                            ),
                            ("marker", marker),
                            ("name", node.name.clone()),
                        ],
                    ),
                    detail: String::new(),
                    source: String::new(),
                }
            })
            .collect();
        if !self.explorer_preview_lines.is_empty() {
            rows.push(PickerRow {
                id: "explorer-preview-divider".into(),
                label: self.message("explorer_preview_title", &[]),
                detail: String::new(),
                source: String::new(),
            });
            rows.extend(
                self.explorer_preview_lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| PickerRow {
                        id: format!("explorer-preview:{index}"),
                        label: line.clone(),
                        detail: String::new(),
                        source: String::new(),
                    }),
            );
        }
        let viewport_rows = usize::from(self.terminal_height.saturating_sub(4)).max(1);
        let maximum = self
            .configuration
            .settings
            .ui
            .explorer
            .maximum_visible_rows
            .min(viewport_rows);
        let scroll = selected.map_or(0, |selected| {
            selected.saturating_sub(maximum.saturating_sub(1))
        });
        self.ui.explorer = Some(PickerUiState {
            title: self.message("explorer_title", &[]),
            query: String::new(),
            rows,
            selected,
            scroll,
            loading: self.explorer_pending.is_some(),
        });
        self.request_explorer_preview();
    }

    fn request_explorer_preview(&mut self) {
        if !self.configuration.settings.ui.explorer.preview_enabled {
            return;
        }
        let Some(node) = self.explorer.selected_node() else {
            return;
        };
        if !matches!(node.kind, ExplorerKind::File) {
            self.explorer_preview_path = None;
            self.explorer_preview_lines.clear();
            return;
        }
        let path = node.path.clone();
        if self.explorer_preview_path.as_ref() == Some(&path) {
            return;
        }
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        let generation = self.explorer_preview_generation;
        self.explorer_preview_path = Some(path.clone());
        self.explorer_preview_lines.clear();
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        if let Ok(id) = self.vfs.read_scoped_bounded(
            path.clone(),
            roots,
            self.configuration
                .settings
                .ui
                .explorer
                .maximum_preview_bytes as u64,
        ) {
            self.pending_vfs
                .insert(id, PendingVfs::ExplorerPreview { path, generation });
        }
    }

    fn open_explorer(&mut self) {
        if self.explorer.root_revision != self.workspace.root_revision()
            || self.explorer.visible().is_empty()
        {
            self.explorer
                .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        }
        self.ui.explorer = Some(PickerUiState {
            title: String::new(),
            query: String::new(),
            rows: Vec::new(),
            selected: Some(0),
            scroll: 0,
            loading: false,
        });
        self.ui.explorer_focused = true;
        self.sync_explorer_ui();
        let root_to_load = self.explorer.selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root_to_load {
            self.load_explorer_node(root);
        }
    }

    fn load_explorer_node(&mut self, node: ExplorerNodeId) {
        if self.explorer_pending.is_some() {
            return;
        }
        let Some(path) = self.explorer.node(node).map(|node| node.path.clone()) else {
            return;
        };
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.directory_service.list(
            path,
            roots,
            self.workspace.root_revision(),
            self.explorer_hidden,
            self.explorer_ignored,
            self.configuration.settings.workspace.maximum_picker_items,
        ) {
            Ok(generation) => {
                self.explorer.begin_load(node, generation);
                self.explorer_pending = Some((node, generation, self.workspace.root_revision()));
                self.sync_explorer_ui();
            }
            Err(error) => self.show_error(error),
        }
    }

    fn explorer_toggle(&mut self) {
        let Some(node) = self.explorer.selected_node().cloned() else {
            return;
        };
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => match node.load {
                ExplorerLoadState::Loaded => {
                    self.explorer.toggle_loaded(node.id);
                    self.sync_explorer_ui();
                }
                _ => self.load_explorer_node(node.id),
            },
            ExplorerKind::File | ExplorerKind::Symlink => self.explorer_open_file(false),
        }
    }

    fn process_explorer_result(&mut self) {
        let Some(result) = self.directory_service.try_result() else {
            return;
        };
        let Some((node, generation, revision)) = self.explorer_pending.take() else {
            return;
        };
        match result {
            Ok(listing)
                if listing.generation == generation
                    && listing.root_revision == revision
                    && revision == self.workspace.root_revision() =>
            {
                let children = listing
                    .entries
                    .into_iter()
                    .map(|entry| ExplorerChild {
                        path: entry.path,
                        name: entry.name,
                        kind: match entry.kind {
                            DirectoryEntryKind::Directory => ExplorerKind::Directory,
                            DirectoryEntryKind::File => ExplorerKind::File,
                            DirectoryEntryKind::Symlink => ExplorerKind::Symlink,
                        },
                    })
                    .collect();
                let selected_loaded_root = self.explorer.selected_id() == Some(node)
                    && self
                        .explorer
                        .node(node)
                        .is_some_and(|node| node.kind == ExplorerKind::Root);
                self.explorer
                    .apply_load(node, generation, revision, children);
                if selected_loaded_root && self.workspace.roots().len() == 1 {
                    self.explorer.select_first_child();
                }
            }
            Ok(_) => {}
            Err(error) => {
                self.explorer.set_error(node, generation);
                self.show_error(error);
            }
        }
        self.sync_explorer_ui();
    }

    fn explorer_open_file(&mut self, split: bool) {
        let Some(node) = self.explorer.selected_node() else {
            return;
        };
        if matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory) {
            self.explorer_toggle();
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.read_scoped(node.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::ExplorerOpen { split });
                self.ui.explorer_focused = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn explorer_mouse_select(&mut self) {
        let Some((column, row)) = self.mouse_position else {
            return;
        };
        let Some(panel_width) = crate::terminal_ui::explorer_panel_width(
            &self.configuration.settings,
            self.terminal_width,
        ) else {
            return;
        };
        if column >= panel_width || row < 3 {
            return;
        }
        let Some(explorer_ui) = self.ui.explorer.as_ref() else {
            return;
        };
        let index = explorer_ui
            .scroll
            .saturating_add(usize::from(row.saturating_sub(3)));
        let Some(id) = explorer_ui
            .rows
            .get(index)
            .and_then(|row| row.id.parse::<u64>().ok())
        else {
            return;
        };
        self.explorer.select(ExplorerNodeId(id));
        self.ui.explorer_focused = true;
        self.sync_explorer_ui();
    }

    fn refresh_explorer(&mut self) {
        self.explorer_pending = None;
        self.explorer_preview_path = None;
        self.explorer_preview_lines.clear();
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.sync_explorer_ui();
    }

    fn open_terminal_after_validation_with_response(
        &mut self,
        profile: &str,
        dap_request: Option<i64>,
    ) {
        let Some(config) = self
            .configuration
            .settings
            .terminals
            .profiles
            .get(profile)
            .cloned()
        else {
            self.show_error(self.message("terminal_unknown", &[("profile", profile.into())]));
            return;
        };
        let Some(root_id) = self.workspace.active_root() else {
            return;
        };
        let Some(root) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let cwd = config.cwd.clone().map_or_else(
            || root.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            },
        );
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        let request = ExecutionPreflightRequest {
            executable: config.program.clone(),
            cwd,
            roots,
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Terminal {
                        profile: profile.into(),
                        config,
                        dap_request,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }
    fn process_execution_preflight(&mut self) {
        let Ok(events) = self.execution_preflight.poll() else {
            return;
        };
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(result) => {
                    let Some(action) = self.pending_execution_preflight.remove(&result.id) else {
                        continue;
                    };
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        continue;
                    }
                    match action {
                        PendingExecutionPreflight::ToolInstall { definition } => {
                            if let Err(error) = self
                                .tool_installer
                                .install_absolute(definition.clone(), result.executable)
                            {
                                if let Some(entry) = self
                                    .tools
                                    .iter_mut()
                                    .find(|entry| entry.definition.id == definition.id)
                                {
                                    entry.status = ToolStatus::Failed(error.to_string());
                                }
                                self.show_error(error)
                            }
                        }
                        PendingExecutionPreflight::Lsp {
                            session_key,
                            definition,
                            root_id,
                            buffer,
                            document,
                            restart,
                        } => self.launch_preflighted_lsp(
                            session_key,
                            definition,
                            root_id,
                            buffer,
                            document,
                            result,
                            restart,
                        ),
                        PendingExecutionPreflight::Task {
                            requested,
                            name,
                            remaining,
                            task,
                            adapter,
                        } => {
                            let policy = &self.configuration.settings.processes;
                            let request = JobRequest {
                                program: result.executable,
                                arguments: task.arguments,
                                cwd: result.cwd,
                                environment: task.environment,
                                root_revision: result.root_revision,
                                stdin: match &adapter {
                                    AdapterContext::Formatter { source, .. }
                                        if name == requested =>
                                    {
                                        Some(source.clone())
                                    }
                                    _ => None,
                                },
                                policy: JobPolicy {
                                    maximum_output_bytes: policy.maximum_output_bytes,
                                    maximum_line_bytes: policy.maximum_line_bytes,
                                    maximum_stdin_bytes: policy.maximum_stdin_bytes,
                                    timeout: Duration::from_millis(
                                        task.timeout_ms.unwrap_or(policy.maximum_timeout_ms),
                                    ),
                                },
                            };
                            match self.jobs.start(request) {
                                Ok(job) => {
                                    self.task_status
                                        .insert(requested.clone(), format!("running:{name}"));
                                    self.job_tasks.insert(
                                        job,
                                        RunningTask {
                                            requested,
                                            name,
                                            root_revision: result.root_revision,
                                            remaining,
                                            adapter,
                                            root: result.root,
                                        },
                                    );
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        PendingExecutionPreflight::DapToolAdapter {
                            definition,
                            program,
                        } => {
                            let request = ExecutionPreflightRequest {
                                executable: program,
                                cwd: result.cwd.clone(),
                                roots: self
                                    .workspace
                                    .roots()
                                    .iter()
                                    .map(|root| root.identity.canonical_path.clone())
                                    .collect(),
                                root_revision: result.root_revision,
                            };
                            match self.execution_preflight.request(request) {
                                Ok(id) => {
                                    self.pending_execution_preflight.insert(
                                        id,
                                        PendingExecutionPreflight::DapToolProgram {
                                            definition,
                                            adapter_executable: result.executable,
                                            cwd: result.cwd,
                                            root_revision: result.root_revision,
                                        },
                                    );
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        PendingExecutionPreflight::DapToolProgram {
                            definition,
                            adapter_executable,
                            cwd,
                            root_revision,
                        } => {
                            let name = definition.name.clone();
                            match self.dap.start_absolute(definition.id,adapter_executable,definition.launch_args,HashMap::new(),BTreeMap::new(),cwd.clone(),"launch".into(),serde_json::json!({"program":result.executable,"cwd":cwd,"stopOnEntry":false})){Ok(())=>{self.ui.notification=Some(self.message("dap_started",&[("adapter",name)]));self.ui.notification_error=false;},Err(error)=>self.show_error(error)}
                            let _ = root_revision;
                        }
                        PendingExecutionPreflight::DapAdapter {
                            name,
                            adapter,
                            session,
                        } => match session.request {
                            crate::configuration::DebugRequestKind::Attach => self
                                .launch_preflighted_dap(
                                    name,
                                    adapter,
                                    session,
                                    result.executable,
                                    result.cwd,
                                    result.root_revision,
                                    None,
                                ),
                            crate::configuration::DebugRequestKind::Launch => {
                                let Some(program) = session.program.clone() else {
                                    self.show_error("launch debug configuration requires program");
                                    continue;
                                };
                                let program = if program.is_absolute() {
                                    program
                                } else {
                                    result.root.join(program)
                                };
                                let request = ExecutionPreflightRequest {
                                    executable: program,
                                    cwd: result.cwd.clone(),
                                    roots: self
                                        .workspace
                                        .roots()
                                        .iter()
                                        .map(|root| root.identity.canonical_path.clone())
                                        .collect(),
                                    root_revision: result.root_revision,
                                };
                                match self.execution_preflight.request(request) {
                                    Ok(id) => {
                                        self.pending_execution_preflight.insert(
                                            id,
                                            PendingExecutionPreflight::DapProgram {
                                                name,
                                                adapter,
                                                session,
                                                adapter_executable: result.executable,
                                                cwd: result.cwd,
                                                root_revision: result.root_revision,
                                            },
                                        );
                                    }
                                    Err(error) => self.show_error(error),
                                }
                            }
                        },
                        PendingExecutionPreflight::DapProgram {
                            name,
                            adapter,
                            session,
                            adapter_executable,
                            cwd,
                            root_revision,
                        } => self.launch_preflighted_dap(
                            name,
                            adapter,
                            session,
                            adapter_executable,
                            cwd,
                            root_revision,
                            Some(result.executable),
                        ),
                        PendingExecutionPreflight::Terminal {
                            profile,
                            config,
                            dap_request,
                        } => {
                            let policy = &self.configuration.settings.terminals;
                            let columns = config.columns;
                            let rows = config.rows;
                            let request = PtyRequest {
                                program: result.executable,
                                arguments: config.arguments,
                                cwd: result.cwd,
                                environment: config.environment,
                                columns: config.columns,
                                rows: config.rows,
                                root_revision: result.root_revision,
                                policy: PtyPolicy {
                                    maximum_output_bytes: policy.maximum_output_bytes,
                                    maximum_input_bytes: policy.maximum_input_bytes,
                                    event_capacity: policy.queue_capacity,
                                    scrollback_bytes: policy.scrollback_bytes,
                                },
                            };
                            match self.ptys.open(request) {
                                Ok(id) => {
                                    self.terminals.insert(
                                        id,
                                        TerminalRuntime {
                                            profile,
                                            root_revision: result.root_revision,
                                            screen: PtyScreen::new(
                                                columns,
                                                rows,
                                                self.configuration
                                                    .settings
                                                    .terminals
                                                    .scrollback_bytes
                                                    .saturating_div(usize::from(columns.max(1)))
                                                    .max(1),
                                            ),
                                            running: true,
                                            exit_code: None,
                                            truncated: false,
                                        },
                                    );
                                    self.active_terminal = Some(id);
                                    if let Some(request) = dap_request {
                                        self.dap_terminal_responses.insert(id, request);
                                    }
                                    self.sync_terminal_panel();
                                }
                                Err(error) => {
                                    if let Some(request_seq) = dap_request {
                                        let _ = self.dap.control(DapControl::ReverseResponse {
                                            request_seq,
                                            success: false,
                                            message: Some(error.to_string()),
                                            process_id: None,
                                        });
                                    }
                                    self.show_error(error)
                                }
                            }
                        }
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    let action = self.pending_execution_preflight.remove(&id);
                    if let Some(PendingExecutionPreflight::Terminal {
                        dap_request: Some(request_seq),
                        ..
                    }) = &action
                    {
                        let _ = self.dap.control(DapControl::ReverseResponse {
                            request_seq: *request_seq,
                            success: false,
                            message: Some(message.clone()),
                            process_id: None,
                        });
                    }
                    if let Some(PendingExecutionPreflight::ToolInstall { definition }) = action
                        && let Some(entry) = self
                            .tools
                            .iter_mut()
                            .find(|entry| entry.definition.id == definition.id)
                    {
                        entry.status = ToolStatus::Failed(message.clone());
                    }
                    if root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }

    fn process_pty_events(&mut self) {
        if let Ok(events) = self.ptys.poll() {
            for event in events {
                match event {
                    PtyEvent::Started { id, root_revision } => {
                        if let Some(request_seq) = self.dap_terminal_responses.remove(&id) {
                            let success = root_revision == self.workspace.root_revision();
                            let _ = self.dap.control(DapControl::ReverseResponse {
                                request_seq,
                                success,
                                message: (!success).then(|| "workspace root changed".into()),
                                process_id: None,
                            });
                        }
                        if root_revision != self.workspace.root_revision() {
                            let _ = self.ptys.close(id);
                        }
                    }
                    PtyEvent::Output {
                        id,
                        text,
                        truncated,
                    } => {
                        if let Some(terminal) = self.terminals.get_mut(&id).filter(|terminal| {
                            terminal.root_revision == self.workspace.root_revision()
                        }) {
                            terminal.screen.feed(&text);
                            terminal.truncated |= truncated;
                        }
                    }
                    PtyEvent::Exited { id, code } => {
                        if let Some(terminal) = self.terminals.get_mut(&id) {
                            terminal.running = false;
                            terminal.exit_code = code;
                        }
                    }
                    PtyEvent::Failed { id, message } => {
                        if let Some(request_seq) = self.dap_terminal_responses.remove(&id) {
                            let _ = self.dap.control(DapControl::ReverseResponse {
                                request_seq,
                                success: false,
                                message: Some(message.clone()),
                                process_id: None,
                            });
                        }
                        if let Some(terminal) = self.terminals.get_mut(&id) {
                            terminal.running = false;
                        }
                        self.show_error(message);
                    }
                }
            }
            self.sync_terminal_panel();
        }
    }
    fn sync_terminal_panel(&mut self) {
        let Some(id) = self.active_terminal else {
            self.ui.terminal_panel = None;
            return;
        };
        let Some(terminal) = self.terminals.get(&id) else {
            self.ui.terminal_panel = None;
            return;
        };
        let screen_lines = terminal.screen.lines();
        let rows = screen_lines
            .iter()
            .rev()
            .take(500)
            .rev()
            .enumerate()
            .map(|(index, line)| PickerRow {
                id: format!("terminal:{}:{index}", id.0),
                label: line.clone(),
                detail: if terminal.truncated {
                    self.message("terminal_truncated", &[])
                } else {
                    String::new()
                },
                source: String::new(),
            })
            .collect();
        self.ui.terminal_panel = Some(PickerUiState {
            title: self.message(
                "terminal_title",
                &[
                    ("id", id.0.to_string()),
                    ("profile", terminal.profile.clone()),
                    (
                        "status",
                        if terminal.running {
                            self.message("terminal_running", &[])
                        } else {
                            self.message(
                                "terminal_exited",
                                &[(
                                    "code",
                                    terminal
                                        .exit_code
                                        .map_or_else(|| "signal".into(), |code| code.to_string()),
                                )],
                            )
                        },
                    ),
                    (
                        "unsupported",
                        terminal.screen.unsupported_sequences().to_string(),
                    ),
                ],
            ),
            query: String::new(),
            rows,
            selected: None,
            scroll: 0,
            loading: terminal.running,
        });
    }

    fn rebuild_matcher_cache(&mut self) {
        self.matcher_cache = self
            .configuration
            .settings
            .tasks
            .iter()
            .filter_map(|(name, task)| {
                task.matcher
                    .as_ref()
                    .and_then(|matcher| regex::Regex::new(&matcher.pattern).ok())
                    .map(|regex| (name.clone(), regex))
            })
            .collect();
    }

    fn plugin_limits(&self) -> PluginLimits {
        let policy = &self.configuration.settings.plugins;
        PluginLimits {
            maximum_component_bytes: policy.maximum_component_bytes,
            maximum_commands: policy.maximum_commands,
            fuel: policy.fuel,
            epoch_deadline_ticks: policy.epoch_deadline_ticks,
            maximum_memory_bytes: policy.maximum_memory_bytes,
            maximum_table_elements: policy.maximum_table_elements,
            maximum_instances: policy.maximum_instances,
        }
    }
    fn load_configured_plugins(&mut self) {
        if self.safe_mode {
            self.show_error(self.message("safe_mode_blocked", &[]));
            return;
        }
        let policy = self.configuration.settings.plugins.clone();
        let store = if policy.store.is_absolute() {
            policy.store
        } else {
            crate::configuration::user_state_directory().join(policy.store)
        };
        let limits = self.plugin_limits();
        for (id, entry) in policy.entries.iter().filter(|(_, entry)| entry.enabled) {
            if let Err(error) = self.plugins.load_from_store(
                id.clone(),
                store.clone(),
                entry.manifest.clone(),
                entry.component.clone(),
                limits.clone(),
            ) {
                self.show_error(self.message(
                    "plugin_load_failed",
                    &[("id", id.clone()), ("diagnostic", error.to_string())],
                ));
            }
        }
    }
    fn begin_package_activation_rollback(&mut self) {
        if self.package_startup_rollback.is_some() || self.package_startup_rollback_attempted {
            return;
        }
        let managed = std::mem::take(&mut self.package_managed_plugins);
        let mut failed = HashMap::new();
        for (id, load) in managed {
            match self.plugins.unload_generation(&id, load) {
                Ok(()) => self.commands.unregister_plugin(&id),
                Err(error) => {
                    failed.insert(id, load);
                    self.show_error(error);
                }
            }
        }
        if !failed.is_empty() {
            self.package_managed_plugins = failed;
            return;
        }
        self.package_startup_pending.clear();
        match self.packages.request(PackageOperation::Rollback) {
            Ok(id) => {
                self.package_startup_rollback_attempted = true;
                self.package_startup_rollback = Some(id)
            }
            Err(error) => self.show_error(error),
        }
    }
    fn handle_plugin_worker_stopped(&mut self, error: crate::plugins::PluginError) {
        if self.plugin_worker_reported {
            return;
        }
        self.plugin_worker_reported = true;
        for plugin in self.plugins.statuses.keys().cloned().collect::<Vec<_>>() {
            self.commands.unregister_plugin(&plugin);
        }
        for (plugin, (_, token)) in self.package_prepared.drain() {
            self.commands.unregister_plugin(&plugin);
            let _ = self.packages.request(PackageOperation::Abort { token });
        }
        for plugin in self
            .package_startup_pending
            .keys()
            .chain(self.package_managed_plugins.keys())
            .cloned()
            .collect::<Vec<_>>()
        {
            self.commands.unregister_plugin(&plugin);
        }
        self.package_startup_pending.clear();
        self.package_managed_plugins.clear();
        for (plugin, _) in self.package_commits.values() {
            self.commands.unregister_plugin(plugin);
        }
        self.logs.record(
            LogLevel::Error,
            "plugins",
            "worker-stopped",
            BTreeMap::from([("error".into(), error.to_string())]),
        );
        self.show_error(self.message(
            "plugin_worker_stopped",
            &[("diagnostic", error.to_string())],
        ));
    }
    fn process_plugin_events(&mut self) {
        let events = match self.plugins.poll() {
            Ok(events) => events,
            Err(error) => {
                self.handle_plugin_worker_stopped(error);
                return;
            }
        };
        for event in events {
            match event {
                PluginEvent::Loaded { load, component } => {
                    let plugin = component.manifest.id.clone();
                    let prepared = self
                        .package_prepared
                        .get(&plugin)
                        .filter(|(candidate, _)| *candidate == load)
                        .map(|(_, token)| *token);
                    if prepared.is_some() {
                        self.package_prepared.remove(&plugin);
                    }
                    if self.package_startup_pending.get(&plugin) == Some(&load) {
                        self.package_startup_pending.remove(&plugin);
                    }
                    self.commands.unregister_plugin(&plugin);
                    for command in &component.activated_commands {
                        let id = format!("plugin.{plugin}.{command}");
                        let Ok(id) = CommandId::new(&id) else {
                            continue;
                        };
                        let title = component
                            .manifest
                            .commands
                            .get(command)
                            .cloned()
                            .unwrap_or_else(|| command.clone());
                        let effect_plugin = plugin.clone();
                        let effect_command = command.clone();
                        let spec = CommandSpec {
                            id,
                            title,
                            description: "WASM component command".into(),
                            arguments: ArgumentSchema::default(),
                            context: ContextRequirement::Buffer,
                            source: CommandSource::Plugin(plugin.clone()),
                        };
                        let _ = self.commands.register(spec, move |_, _| {
                            Ok(CommandOutcome {
                                transaction: None,
                                effects: vec![AppEffect::PluginExecute {
                                    plugin: effect_plugin.clone(),
                                    command: effect_command.clone(),
                                }],
                                message: None,
                                follow_up: Vec::new(),
                            })
                        });
                    }
                    if let Some(token) = prepared {
                        match self.packages.request(PackageOperation::Commit { token }) {
                            Ok(request) => {
                                self.package_commits.insert(request, (plugin.clone(), load));
                            }
                            Err(error) => {
                                let _ = self.plugins.disable_generation(&plugin, load);
                                self.commands.unregister_plugin(&plugin);
                                let _ = self.packages.request(PackageOperation::Abort { token });
                                self.show_error(error)
                            }
                        }
                    }
                    self.logs.record(
                        LogLevel::Info,
                        "plugins",
                        "active",
                        BTreeMap::from([
                            ("plugin".into(), plugin),
                            (
                                "commands".into(),
                                component.activated_commands.len().to_string(),
                            ),
                        ]),
                    );
                }
                PluginEvent::Executed(execution) => self.apply_plugin_execution(execution),
                PluginEvent::Failed {
                    load,
                    plugin,
                    error,
                } => {
                    let prepared = self
                        .package_prepared
                        .get(&plugin)
                        .filter(|(candidate, _)| *candidate == load)
                        .map(|(_, token)| *token);
                    if let Some(token) = prepared {
                        self.package_prepared.remove(&plugin);
                        let _ = self.packages.request(PackageOperation::Abort { token });
                    } else if self.package_startup_pending.get(&plugin) == Some(&load) {
                        self.package_startup_pending.remove(&plugin);
                        self.begin_package_activation_rollback();
                    }
                    self.commands.unregister_plugin(&plugin);
                    self.logs.record(
                        LogLevel::Error,
                        "plugins",
                        "failed",
                        BTreeMap::from([
                            ("plugin".into(), plugin),
                            ("error".into(), error.to_string()),
                        ]),
                    );
                }
            }
        }
    }

    fn apply_plugin_execution(&mut self, execution: PluginExecution) {
        for message in execution.messages {
            self.ui.notification = Some(message);
            self.ui.notification_error = false;
        }
        let mut documents = Vec::new();
        for proposal in execution.proposals {
            let Some((buffer, state)) = self
                .state
                .buffers
                .iter()
                .find(|(id, _)| id.data().as_ffi() == proposal.buffer)
            else {
                continue;
            };
            let revision = BufferRevision(proposal.revision);
            if state.text.revision() != revision {
                continue;
            }
            let Some(path) = state.path.as_ref() else {
                continue;
            };
            let codec = PositionCodec::new(&state.text);
            let mut edits = Vec::new();
            for edit in proposal.edits {
                let (Ok(start), Ok(end)) = (usize::try_from(edit.start), usize::try_from(edit.end))
                else {
                    continue;
                };
                let (Ok((start_line, start_character)), Ok((end_line, end_character))) = (
                    codec.byte_to_lsp(ByteOffset(start), PositionEncoding::Utf8),
                    codec.byte_to_lsp(ByteOffset(end), PositionEncoding::Utf8),
                ) else {
                    continue;
                };
                edits.push(LspTextEdit {
                    range: LspRange {
                        start: LspPosition {
                            line: start_line,
                            character: start_character,
                        },
                        end: LspPosition {
                            line: end_line,
                            character: end_character,
                        },
                    },
                    new_text: edit.replacement,
                });
            }
            if !edits.is_empty() {
                documents.push(LspDocumentEdit {
                    uri: file_uri(path),
                    version: Some(revision.0),
                    edits,
                });
            }
            let _ = buffer;
        }
        if !documents.is_empty() {
            self.begin_workspace_proposal(
                format!("plugin {}: {}", execution.plugin, execution.command),
                LspWorkspaceEdit {
                    resources: Vec::new(),
                    documents,
                },
                None,
                PositionEncoding::Utf8,
            );
        }
        self.logs.record(
            LogLevel::Info,
            "plugins",
            "executed",
            BTreeMap::from([
                ("plugin".into(), execution.plugin),
                ("command".into(), execution.command),
                ("generation".into(), execution.generation.to_string()),
            ]),
        );
    }

    fn install_package_after_validation(
        &mut self,
        package_manifest: PathBuf,
        plugin_manifest: PathBuf,
        component: PathBuf,
        update: bool,
    ) {
        let root = self.state.workspace_root.clone();
        let absolute = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        };
        let operation = PackageOperation::Install {
            package_manifest: absolute(package_manifest),
            plugin_manifest: absolute(plugin_manifest),
            component: absolute(component),
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            limits: self.plugin_limits(),
            update,
        };
        if let Err(error) = self.packages.request(operation) {
            self.show_error(error)
        }
    }

    fn request_registry(&mut self, request: RegistryRequest, install: bool) {
        match self.registry.request(request) {
            Ok(id) => {
                self.registry_intents.insert(id, install);
            }
            Err(error) => self.show_error(error),
        }
    }
    fn process_registry_events(&mut self) {
        let Ok(events) = self.registry.poll() else {
            return;
        };
        for event in events {
            let install = self.registry_intents.remove(&event.id).unwrap_or(false);
            match event.result {
                Ok(RegistryResult::Search(result)) => {
                    let items = result
                        .packages
                        .into_iter()
                        .enumerate()
                        .map(|(index, package)| {
                            (
                                PickerItem {
                                    id: PickerItemId(format!(
                                        "registry:{index}:{}/{}",
                                        package.publisher, package.name
                                    )),
                                    label: format!("{}/{}", package.publisher, package.name),
                                    detail: format!(
                                        "{}{}",
                                        package.version,
                                        if package.yanked { " · yanked" } else { "" }
                                    ),
                                    source: package.description.clone(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::RegistrySummary(package),
                            )
                        })
                        .collect();
                    self.install_picker(self.message("package_registry_search_title", &[]), items);
                }
                Ok(RegistryResult::Show(metadata)) => {
                    if install {
                        self.request_registry(RegistryRequest::Fetch { metadata }, true)
                    } else {
                        let label = format!("{}/{}", metadata.publisher, metadata.name);
                        let detail = format!("{} · {}", metadata.version, metadata.artifact_sha256);
                        self.install_picker(
                            self.message("package_registry_show_title", &[]),
                            vec![(
                                PickerItem {
                                    id: PickerItemId(format!("registry-show:{label}")),
                                    label,
                                    detail,
                                    source: metadata.artifact_path.clone(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::RegistryMetadata(metadata),
                            )],
                        );
                    }
                }
                Ok(RegistryResult::Artifact { metadata, bytes }) => {
                    if self.pending_registry_artifact.is_some() {
                        self.show_error("another registry artifact is awaiting validation")
                    } else {
                        self.pending_registry_artifact = Some((metadata, bytes));
                        self.defer_trusted_execution(DeferredExecution::PrepareRegistryArtifact);
                        if self.execution_validation.is_none() {
                            self.pending_registry_artifact = None;
                        }
                    }
                }
                Err(error) => self.show_error(self.message(
                    "package_registry_failed",
                    &[("diagnostic", error.to_string())],
                )),
            }
        }
    }

    fn process_package_events(&mut self) {
        if let Ok(events) = self.packages.poll() {
            for event in events {
                match event {
                    PackageServiceEvent::Completed(mut result) => {
                        let prepared_operation = result.prepared.is_some();
                        let activation_operation = !result.activations.is_empty();
                        if let Some(prepared) = result.prepared.take() {
                            let id = prepared.plugin.id.clone();
                            let token = prepared.token;
                            let store =
                                crate::configuration::user_state_directory().join("packages/store");
                            if self.plugins.load_id(&id).is_some() {
                                let _ = self.packages.request(PackageOperation::Abort { token });
                                self.show_error(
                                    self.message("package_plugin_conflict", &[("plugin", id)]),
                                );
                                continue;
                            }
                            match self.plugins.load(
                                store,
                                prepared.object,
                                prepared.plugin,
                                self.plugin_limits(),
                            ) {
                                Ok(load) => {
                                    self.package_prepared.insert(id.clone(), (load, token));
                                    self.ui.notification = Some(self.message(
                                        "package_prepared",
                                        &[("package", prepared.manifest.name)],
                                    ));
                                    self.ui.notification_error = false;
                                }
                                Err(error) => {
                                    let _ =
                                        self.packages.request(PackageOperation::Abort { token });
                                    self.show_error(error)
                                }
                            }
                        }
                        for (manifest, path) in std::mem::take(&mut result.activations) {
                            let id = manifest.id.clone();
                            if self.plugins.load_id(&id).is_some() {
                                self.show_error(
                                    self.message("package_plugin_conflict", &[("plugin", id)]),
                                );
                                self.begin_package_activation_rollback();
                                break;
                            }
                            let store =
                                crate::configuration::user_state_directory().join("packages/store");
                            match self
                                .plugins
                                .load(store, path, manifest, self.plugin_limits())
                            {
                                Ok(load) => {
                                    self.package_startup_pending.insert(id.clone(), load);
                                    self.package_managed_plugins.insert(id, load);
                                }
                                Err(error) => {
                                    self.show_error(error);
                                    self.begin_package_activation_rollback();
                                    break;
                                }
                            }
                        }
                        if let Some((plugin, load)) = self.package_commits.remove(&result.id) {
                            if self.plugins.load_id(&plugin) == Some(load) {
                                self.package_managed_plugins.insert(plugin, load);
                            } else {
                                self.package_startup_rollback_attempted = false;
                                self.begin_package_activation_rollback();
                            }
                        }
                        if self.package_startup_request == Some(result.id) {
                            self.package_startup_request = None;
                        }
                        let reconcile = matches!(result.operation.as_str(), "rollback" | "remove");
                        if self.package_startup_rollback == Some(result.id) {
                            self.package_startup_rollback = None;
                        }
                        if reconcile {
                            let managed = std::mem::take(&mut self.package_managed_plugins);
                            let mut failed = HashMap::new();
                            for (id, load) in managed {
                                match self.plugins.unload_generation(&id, load) {
                                    Ok(()) => self.commands.unregister_plugin(&id),
                                    Err(error) => {
                                        failed.insert(id, load);
                                        self.show_error(error);
                                    }
                                }
                            }
                            self.package_managed_plugins = failed;
                            self.package_startup_pending.clear();
                            if !self.package_managed_plugins.is_empty() {
                                self.begin_package_activation_rollback();
                                continue;
                            }
                            match self.packages.request(PackageOperation::Activations) {
                                Ok(id) => self.package_startup_request = Some(id),
                                Err(error) => {
                                    self.show_error(error);
                                    self.begin_package_activation_rollback();
                                }
                            }
                        }
                        if let Some(lock) = result.lock {
                            let items = lock
                                .packages
                                .into_iter()
                                .enumerate()
                                .map(|(index, package)| {
                                    (
                                        PickerItem {
                                            id: PickerItemId(format!(
                                                "package:{index}:{}",
                                                package.name
                                            )),
                                            label: package.name,
                                            detail: package.version,
                                            source: "package".into(),
                                            action: PickerAction::None,
                                        },
                                        PickerPayload::Command(String::new()),
                                    )
                                })
                                .collect();
                            self.install_picker(self.message("package_picker_title", &[]), items);
                        } else if !prepared_operation && !activation_operation {
                            self.ui.notification = Some(self.message(
                                "package_operation_complete",
                                &[
                                    ("operation", result.operation),
                                    (
                                        "count",
                                        result.verified.or(result.removed).unwrap_or(0).to_string(),
                                    ),
                                ],
                            ));
                            self.ui.notification_error = false;
                        }
                    }
                    PackageServiceEvent::Failed {
                        id,
                        operation,
                        error,
                    } => {
                        if self.package_startup_request == Some(id) {
                            self.package_startup_request = None;
                            self.begin_package_activation_rollback();
                        }
                        if let Some((plugin, load)) = self.package_commits.remove(&id) {
                            if self.package_managed_plugins.get(&plugin) == Some(&load) {
                                self.package_managed_plugins.remove(&plugin);
                            }
                            let _ = self.plugins.disable_generation(&plugin, load);
                            self.commands.unregister_plugin(&plugin);
                        }
                        self.logs.record(
                            LogLevel::Error,
                            "packages",
                            "operation-failed",
                            BTreeMap::from([
                                ("operation".into(), operation.clone()),
                                ("error".into(), error.to_string()),
                            ]),
                        );
                        self.show_error(self.message(
                            "package_operation_failed",
                            &[("operation", operation), ("diagnostic", error.to_string())],
                        ));
                    }
                }
            }
        }
    }

    fn start_task_after_validation(&mut self, name: &str) {
        self.start_task_plan(name, AdapterContext::Plain);
    }
    fn start_task_plan(&mut self, name: &str, adapter: AdapterContext) {
        if self.job_tasks.values().any(|task| task.requested == name) {
            self.show_error(self.message("task_already_running", &[("name", name.into())]));
            return;
        }
        let Some(mut plan) = task_plan(name, &self.configuration.settings.tasks) else {
            self.show_error(self.message("task_unknown", &[("name", name.into())]));
            return;
        };
        self.task_output.insert(name.to_owned(), Vec::new());
        self.task_output_rows.insert(name.to_owned(), Vec::new());
        self.task_status.insert(name.to_owned(), "queued".into());
        self.start_task_step(name.to_owned(), &mut plan, adapter);
    }

    fn start_task_step(
        &mut self,
        requested: String,
        plan: &mut Vec<String>,
        adapter: AdapterContext,
    ) {
        if self.job_tasks.len() >= self.configuration.settings.processes.maximum_running
            || plan.is_empty()
        {
            return;
        }
        let name = plan.remove(0);
        let Some(task) = self.configuration.settings.tasks.get(&name).cloned() else {
            return;
        };
        let Some(root_id) = self.workspace.active_root() else {
            self.show_error(self.message("task_no_root", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let cwd = task.cwd.clone().map_or_else(
            || root_path.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root_path.join(path)
                }
            },
        );
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        let request = ExecutionPreflightRequest {
            executable: task.executable.clone(),
            cwd,
            roots,
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::Task {
                        requested,
                        name,
                        remaining: plan.clone(),
                        task,
                        adapter,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    fn apply_formatter_result(&mut self, task: &RunningTask) {
        let AdapterContext::Formatter {
            buffer, revision, ..
        } = task.adapter
        else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(replacement) = String::from_utf8(bytes.clone()) else {
            self.show_error(self.message("formatter_invalid_utf8", &[]));
            return;
        };
        let Some(state) = self.state.buffers.get(buffer) else {
            return;
        };
        if state.text.revision() != revision {
            self.show_error(self.message("formatter_stale", &[]));
            return;
        }
        let end = ByteOffset(state.text.len_bytes());
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision { buffer, revision })
            .with_precondition(Precondition::BufferWritable(buffer))
            .with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    revision,
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), end).expect("ordered"),
                        replacement,
                    )],
                ),
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.ui.notification = Some(self.message("formatter_applied", &[]));
            self.ui.notification_error = false;
        }
    }

    fn apply_linter_result(&mut self, task: &RunningTask) {
        let AdapterContext::Linter { matcher } = &task.adapter else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some(regex) = self.matcher_cache.get(&task.requested) else {
            return;
        };
        let Ok(owner) = DiagnosticOwner::new(format!("task:{}", task.requested)) else {
            return;
        };
        let mut grouped: BTreeMap<String, Vec<Diagnostic>> = BTreeMap::new();
        for captures in regex
            .captures_iter(text)
            .take(self.configuration.settings.workspace.maximum_diagnostics)
        {
            let Some(path) = captures
                .name(&matcher.path_group)
                .map(|value| PathBuf::from(value.as_str()))
            else {
                continue;
            };
            let path = if path.is_absolute() {
                path
            } else {
                task.root.join(path)
            };
            if !path.starts_with(&task.root) {
                continue;
            }
            let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            else {
                continue;
            };
            let line = captures
                .name(&matcher.line_group)
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let column = matcher
                .column_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let Some(start) = buffer
                .text
                .line_to_byte(line)
                .ok()
                .map(|offset| ByteOffset((offset.get() + column).min(buffer.text.len_bytes())))
            else {
                continue;
            };
            if buffer.text.validate_offset(start).is_err() {
                continue;
            }
            let severity = matcher
                .severity_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .map(|value| value.as_str())
                .map_or(DiagnosticSeverity::Error, |value| {
                    match value.to_ascii_lowercase().as_str() {
                        "warning" | "warn" => DiagnosticSeverity::Warning,
                        "info" | "information" => DiagnosticSeverity::Information,
                        "hint" => DiagnosticSeverity::Hint,
                        _ => DiagnosticSeverity::Error,
                    }
                });
            let message = captures
                .name(&matcher.message_group)
                .map(|value| value.as_str().chars().take(4096).collect())
                .unwrap_or_default();
            let uri = crate::languages::file_uri(&path);
            grouped.entry(uri.clone()).or_default().push(Diagnostic {
                uri,
                version: Some(buffer.text.revision()),
                range: ByteRange::new(start, start).expect("empty range"),
                severity,
                source: task.requested.clone(),
                code: None,
                message,
                related: Vec::new(),
                tags: Vec::new(),
                fixes: Vec::new(),
                timestamp: crate::clock::system_time(),
            });
        }
        let mut unopened = BTreeMap::<PathBuf, Vec<RawLinterDiagnostic>>::new();
        for captures in regex
            .captures_iter(text)
            .take(self.configuration.settings.workspace.maximum_diagnostics)
        {
            let Some(raw_path) = captures
                .name(&matcher.path_group)
                .map(|value| PathBuf::from(value.as_str()))
            else {
                continue;
            };
            let path = if raw_path.is_absolute() {
                raw_path
            } else {
                task.root.join(raw_path)
            };
            if !path.starts_with(&task.root)
                || self
                    .state
                    .buffers
                    .iter()
                    .any(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                continue;
            }
            let line = captures
                .name(&matcher.line_group)
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let column = matcher
                .column_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let severity = matcher
                .severity_group
                .as_ref()
                .and_then(|name| captures.name(name))
                .map(|value| value.as_str().to_ascii_lowercase())
                .map_or(DiagnosticSeverity::Error, |value| match value.as_str() {
                    "warning" | "warn" => DiagnosticSeverity::Warning,
                    "info" | "information" => DiagnosticSeverity::Information,
                    "hint" => DiagnosticSeverity::Hint,
                    _ => DiagnosticSeverity::Error,
                });
            let message = captures
                .name(&matcher.message_group)
                .map(|value| value.as_str().chars().take(4096).collect())
                .unwrap_or_default();
            unopened.entry(path).or_default().push(RawLinterDiagnostic {
                line,
                column,
                severity,
                message,
            });
        }
        if !unopened.is_empty() {
            let generation = self.next_linter_generation;
            self.next_linter_generation = self.next_linter_generation.saturating_add(1);
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect::<Vec<_>>();
            let mut waiting = 0;
            for (path, raw) in unopened {
                match self.vfs.read_scoped(path, roots.clone()) {
                    Ok(id) => {
                        self.pending_vfs
                            .insert(id, PendingVfs::LinterRead { generation, raw });
                        waiting += 1
                    }
                    Err(error) => {
                        self.logs.record(
                            LogLevel::Error,
                            "linter",
                            "source-read-queue-failed",
                            BTreeMap::from([("error".into(), error.to_string())]),
                        );
                        return;
                    }
                }
            }
            self.pending_linter_builds.insert(
                generation,
                PendingLinterBuild {
                    owner,
                    root_revision: self.workspace.root_revision(),
                    waiting,
                    failed: false,
                    grouped,
                },
            );
            return;
        }
        self.diagnostics.clear_owner(&owner);
        for (uri, items) in grouped {
            let current = items.first().and_then(|item| item.version);
            let _ = self
                .diagnostics
                .replace(owner.clone(), uri, current, current, items);
        }
        self.sync_ui_diagnostics();
    }

    fn finish_linter_build(&mut self, generation: u64) {
        let Some(build) = self.pending_linter_builds.remove(&generation) else {
            return;
        };
        if build.failed || build.root_revision != self.workspace.root_revision() {
            return;
        }
        self.diagnostics.clear_owner(&build.owner);
        for (uri, items) in build.grouped {
            let current = items.first().and_then(|item| item.version);
            let _ = self
                .diagnostics
                .replace(build.owner.clone(), uri, current, current, items);
        }
        self.sync_ui_diagnostics();
    }

    fn apply_test_result(&mut self, task: &RunningTask) {
        let AdapterContext::Test { matcher } = &task.adapter else {
            return;
        };
        let Some(bytes) = self.task_output.get(&task.requested) else {
            return;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some(regex) = self.matcher_cache.get(&task.requested) else {
            return;
        };
        let cases = regex
            .captures_iter(text)
            .take(10_000)
            .map(|captures| TestCaseResult {
                name: matcher
                    .name_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(512).collect())
                    .unwrap_or_else(|| "test".into()),
                status: matcher
                    .status_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(32).collect())
                    .unwrap_or_else(|| "unknown".into()),
                duration: matcher
                    .duration_group
                    .as_ref()
                    .and_then(|name| captures.name(name))
                    .map(|value| value.as_str().chars().take(64).collect())
                    .unwrap_or_default(),
                message: captures
                    .name(&matcher.message_group)
                    .map(|value| value.as_str().chars().take(2048).collect())
                    .unwrap_or_default(),
                source: captures
                    .name(&matcher.path_group)
                    .map(|value| PathBuf::from(value.as_str()))
                    .map(|path| MatcherSource {
                        path: if path.is_absolute() {
                            path
                        } else {
                            task.root.join(path)
                        },
                        line: captures
                            .name(&matcher.line_group)
                            .and_then(|value| value.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                        column: matcher
                            .column_group
                            .as_ref()
                            .and_then(|name| captures.name(name))
                            .and_then(|value| value.as_str().parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1),
                    })
                    .filter(|source| source.path.starts_with(&task.root)),
            })
            .collect();
        self.test_results.insert(task.requested.clone(), cases);
    }
    fn show_task_picker(&mut self) {
        let items = self
            .task_status
            .iter()
            .map(|(name, status)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("task:{name}")),
                        label: name.clone(),
                        detail: status.clone(),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_tasks_title", &[]), items);
    }
    fn show_output_picker(&mut self, name: &str) {
        let items = self
            .task_output_rows
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("output:{name}:{index}")),
                        label: row.text.clone(),
                        detail: format!(
                            "{:?}{}",
                            row.stream,
                            if row.truncated { " · truncated" } else { "" }
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Command(String::new()),
                )
            })
            .collect();
        self.install_picker(
            self.message("picker_output_title", &[("name", name.into())]),
            items,
        );
    }

    fn show_test_picker(&mut self, name: &str) {
        let items = self
            .test_results
            .get(name)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, case)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("test:{name}:{index}")),
                        label: case.name.clone(),
                        detail: format!("{} {}", case.status, case.duration),
                        source: case.message.clone(),
                        action: PickerAction::None,
                    },
                    case.source.clone().map_or(
                        PickerPayload::Command(String::new()),
                        PickerPayload::MatcherSource,
                    ),
                )
            })
            .collect();
        self.install_picker(
            self.message("picker_tests_title", &[("name", name.into())]),
            items,
        );
    }

    fn process_job_events(&mut self) {
        let revision = self.workspace.root_revision();
        for event in self.jobs.poll() {
            match event {
                JobEvent::Output {
                    job,
                    stream,
                    bytes,
                    truncated,
                } => {
                    if truncated {
                        self.task_truncated.insert(job);
                    }
                    if let Some(task) = self
                        .job_tasks
                        .get(&job)
                        .filter(|task| task.root_revision == revision)
                    {
                        if task.name == task.requested
                            && matches!(task.adapter, AdapterContext::Formatter { .. })
                            && stream == JobStream::Stderr
                        {
                            continue;
                        }
                        let output = self.task_output.entry(task.requested.clone()).or_default();
                        let remaining = self
                            .configuration
                            .settings
                            .processes
                            .maximum_output_bytes
                            .saturating_sub(output.len());
                        let accepted = &bytes[..bytes.len().min(remaining)];
                        output.extend_from_slice(accepted);
                        let rows = self
                            .task_output_rows
                            .entry(task.requested.clone())
                            .or_default();
                        for line in String::from_utf8_lossy(accepted).lines().take(1024) {
                            rows.push(TaskOutputRecord {
                                stream,
                                text: line
                                    .chars()
                                    .filter(|character| !character.is_control())
                                    .take(4096)
                                    .collect(),
                                truncated,
                            });
                        }
                        if rows.len() > 10_000 {
                            rows.drain(..rows.len() - 10_000);
                        }
                    }
                }
                JobEvent::Exited { job, code, .. } => {
                    if let Some(task) = self.job_tasks.remove(&job) {
                        let truncated = self.task_truncated.remove(&job);
                        if task.root_revision != revision {
                            continue;
                        }
                        if code == Some(0) && !task.remaining.is_empty() {
                            let mut remaining = task.remaining;
                            self.start_task_step(task.requested, &mut remaining, task.adapter);
                        } else {
                            if code == Some(0) && !truncated {
                                self.apply_formatter_result(&task);
                                self.apply_linter_result(&task);
                                self.apply_test_result(&task);
                            } else if matches!(task.adapter, AdapterContext::Formatter { .. })
                                && truncated
                            {
                                self.show_error(self.message("formatter_truncated", &[]));
                            }
                            self.task_status.insert(
                                task.requested.clone(),
                                if code == Some(0) {
                                    "succeeded".into()
                                } else {
                                    "failed".into()
                                },
                            );
                            self.ui.notification = Some(self.message(
                                "task_exited",
                                &[
                                    ("name", task.name),
                                    (
                                        "code",
                                        code.map_or_else(
                                            || "signal".into(),
                                            |value| value.to_string(),
                                        ),
                                    ),
                                ],
                            ));
                            self.ui.notification_error = code != Some(0);
                        }
                    }
                }
                JobEvent::Started { job, root_revision } => {
                    if root_revision != revision {
                        let _ = self.jobs.cancel(job);
                    }
                }
                JobEvent::Failed { job, message } => {
                    self.job_tasks.remove(&job);
                    self.show_error(self.message("task_failed", &[("diagnostic", message)]));
                }
            }
        }
    }

    fn effective_large_file(&self, buffer: crate::editing::BufferId) -> bool {
        match self
            .large_file_overrides
            .get(&buffer)
            .copied()
            .unwrap_or(LargeFileOverride::Auto)
        {
            LargeFileOverride::On => true,
            LargeFileOverride::Off => false,
            LargeFileOverride::Auto => self.state.buffers.get(buffer).is_some_and(|buffer| {
                self.configuration.settings.editor.large_file.enabled
                    && (buffer.text.len_bytes()
                        >= self.configuration.settings.editor.large_file.byte_threshold
                        || buffer.text.len_lines()
                            >= self.configuration.settings.editor.large_file.line_threshold)
            }),
        }
    }

    fn process_large_file_modes(&mut self) {
        for (buffer, state) in &self.state.buffers {
            if !self.large_file_overrides.contains_key(&buffer)
                && let Some(mode) = state
                    .path
                    .as_ref()
                    .and_then(|path| self.persisted_large_file_overrides.get(path))
                    .copied()
            {
                self.large_file_overrides.insert(buffer, mode);
            }
        }
        let live = self.state.buffers.keys().collect::<Vec<_>>();
        self.large_file_overrides
            .retain(|buffer, _| live.contains(buffer));
        self.ui.large_file_buffers = live
            .into_iter()
            .filter(|buffer| self.effective_large_file(*buffer))
            .collect();
    }

    fn persistence_policy(&self) -> PersistencePolicy {
        PersistencePolicy {
            maximum_file_bytes: self.configuration.settings.persistence.maximum_file_bytes,
            maximum_entries: self.configuration.settings.persistence.maximum_entries,
        }
    }

    fn load_persistence(&mut self) {
        let configured = &self.configuration.settings.persistence;
        if !configured.enabled || !configured.restore_on_startup || !self.persistence_path.exists()
        {
            return;
        }
        if let Ok(id) = self
            .persistence
            .read(self.persistence_path.clone(), self.persistence_policy())
        {
            self.persistence_pending = Some(id);
        }
    }

    fn capture_session(&self) -> Option<PersistedSession> {
        let mut tabs = Vec::new();
        let mut active_tab = 0_usize;
        for (index, (tab_id, tab)) in self.state.tabs.iter().enumerate() {
            let Some(layout) = capture_persisted_layout(&self.state, &tab.root) else {
                continue;
            };
            if Some(tab_id) == self.state.active_tab {
                active_tab = tabs.len();
            }
            let focused_path = self
                .state
                .panes
                .get(tab.focused)
                .and_then(|pane| self.state.views.get(pane.view))
                .and_then(|view| self.state.buffers.get(view.buffer))
                .and_then(|buffer| buffer.path.clone());
            tabs.push(PersistedTab {
                layout,
                focused_path,
            });
            let _ = index;
        }
        (!tabs.is_empty()).then(|| PersistedSession {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            tabs,
            active_tab,
        })
    }

    fn save_persistence(&mut self) {
        let configured = &self.configuration.settings.persistence;
        if !configured.enabled || !configured.save_on_exit || self.persistence_pending.is_some() {
            return;
        }
        let state = PersistedState {
            version: STATE_VERSION,
            session: self.capture_session(),
            recent_files: self.recent_files.clone(),
            marks: self.marks.clone(),
            jumps: self.jump_list.clone(),
            changes: self.change_list.clone(),
            command_history: if configured.persist_command_history {
                self.command_history.clone()
            } else {
                Vec::new()
            },
            search_history: if configured.persist_search_history {
                self.search_history.clone()
            } else {
                Vec::new()
            },
            breakpoints: self
                .dap_breakpoints
                .values()
                .flatten()
                .take(self.configuration.settings.debug.maximum_breakpoints)
                .map(|row| PersistedBreakpoint {
                    path: row.path.clone(),
                    line: row.line,
                    column: row.column,
                    condition: row.condition.clone(),
                    hit_condition: row.hit_condition.clone(),
                    log_message: row.log_message.clone(),
                })
                .collect(),
            large_file_overrides: self
                .large_file_overrides
                .iter()
                .filter_map(|(id, mode)| {
                    let path = self.state.buffers.get(*id)?.path.clone()?;
                    Some(PersistedLargeFileOverride {
                        path,
                        mode: mode_name_large(*mode).into(),
                    })
                })
                .take(configured.maximum_entries)
                .collect(),
            watches: self
                .dap_watches
                .iter()
                .take(self.configuration.settings.debug.maximum_watches)
                .map(|expression| PersistedWatch {
                    expression: expression.clone(),
                })
                .collect(),
            metadata: BTreeMap::new(),
        };
        if let Ok(id) = self.persistence.write(
            self.persistence_path.clone(),
            state,
            self.persistence_policy(),
        ) {
            self.persistence_pending = Some(id);
        }
    }

    fn process_persistence_events(&mut self) {
        while let Ok(Some(event)) = self.persistence.try_event() {
            match event {
                PersistenceEvent::Loaded { generation, state }
                    if self.persistence_pending == Some(generation) =>
                {
                    let session = state.session.clone();
                    let configured = &self.configuration.settings.persistence;
                    self.recent_files = state
                        .recent_files
                        .into_iter()
                        .take(configured.recent_files)
                        .collect();
                    self.marks = state.marks.into_iter().take(configured.marks).collect();
                    self.jump_list = state.jumps.into_iter().take(configured.jumps).collect();
                    self.change_list = state.changes.into_iter().take(configured.changes).collect();
                    self.dap_breakpoints.clear();
                    for row in
                        state
                            .breakpoints
                            .into_iter()
                            .take(self.configuration.settings.debug.maximum_breakpoints)
                            .filter(|row| {
                                row.path.is_absolute()
                                    && self.workspace.roots().iter().any(|root| {
                                        row.path.starts_with(&root.identity.canonical_path)
                                    })
                            })
                    {
                        self.dap_breakpoints
                            .entry(row.path.clone())
                            .or_default()
                            .push(DapSourceBreakpoint {
                                path: row.path,
                                line: row.line,
                                column: row.column,
                                condition: row.condition,
                                hit_condition: row.hit_condition,
                                log_message: row.log_message,
                            });
                    }
                    self.persisted_large_file_overrides = state
                        .large_file_overrides
                        .into_iter()
                        .filter_map(|entry| {
                            let mode = match entry.mode.as_str() {
                                "on" => LargeFileOverride::On,
                                "off" => LargeFileOverride::Off,
                                "auto" => LargeFileOverride::Auto,
                                _ => return None,
                            };
                            Some((entry.path, mode))
                        })
                        .collect();
                    self.dap_watches = state
                        .watches
                        .into_iter()
                        .take(self.configuration.settings.debug.maximum_watches)
                        .map(|watch| watch.expression)
                        .collect();
                    if configured.persist_command_history {
                        self.command_history = state.command_history;
                    }
                    if configured.persist_search_history {
                        self.search_history = state.search_history;
                    }
                    self.persistence_pending = None;
                    let command_excess = self
                        .command_history
                        .len()
                        .saturating_sub(configured.maximum_entries);
                    self.command_history.drain(..command_excess);
                    let search_excess = self
                        .search_history
                        .len()
                        .saturating_sub(configured.maximum_entries);
                    self.search_history.drain(..search_excess);
                    if let Some(session) = session {
                        self.restore_session(session);
                    }
                }
                PersistenceEvent::Saved { generation }
                    if self.persistence_pending == Some(generation) =>
                {
                    self.persistence_pending = None
                }
                PersistenceEvent::Failed { generation, error }
                    if self.persistence_pending == Some(generation) =>
                {
                    self.persistence_pending = None;
                    self.ui.config_diagnostic =
                        Some(error.to_string().chars().take(1024).collect());
                }
                _ => {}
            }
        }
    }

    fn restore_session(&mut self, session: PersistedSession) {
        if self.safe_mode {
            return;
        }
        if self.session_restore.is_some() {
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        let mut panes = Vec::new();
        for tab in &session.tabs {
            collect_persisted_panes(&tab.layout, &mut panes)
        }
        let paths = panes
            .into_iter()
            .map(|pane| pane.path)
            .filter(|path| roots.iter().any(|root| path.starts_with(root)))
            .take(self.configuration.settings.persistence.maximum_entries)
            .collect::<BTreeSet<_>>();
        if paths.is_empty() {
            return;
        }
        let mut waiting = 0;
        for path in &paths {
            if let Ok(id) = self.vfs.read_scoped(path.clone(), roots.clone()) {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::SessionRestore {
                        requested_path: path.clone(),
                    },
                );
                waiting += 1
            } else {
                return;
            }
        }
        self.session_restore = Some(PendingSessionRestore {
            session,
            waiting,
            loaded: HashMap::new(),
            failed: false,
        });
    }

    fn finish_session_restore(&mut self) {
        let Some(restore) = self.session_restore.take() else {
            return;
        };
        if restore.failed || restore.waiting != 0 {
            return;
        }
        let mut candidate = EditorState::new(self.state.workspace_root.clone());
        candidate.policy = self.state.policy;
        let mut tabs = Vec::new();
        for tab in &restore.session.tabs {
            let Some((layout, panes)) =
                build_restored_layout(&mut candidate, &tab.layout, &restore.loaded)
            else {
                return;
            };
            let Some(focused) = tab
                .focused_path
                .as_ref()
                .and_then(|path| {
                    panes
                        .iter()
                        .find(|(candidate, _)| candidate == path)
                        .map(|(_, pane)| *pane)
                })
                .or_else(|| panes.first().map(|(_, pane)| *pane))
            else {
                return;
            };
            let tab_id = candidate.tabs.insert(crate::editing::Tab {
                root: layout,
                focused,
            });
            tabs.push((tab_id, focused));
        }
        if tabs.is_empty() {
            return;
        }
        let active = restore.session.active_tab.min(tabs.len() - 1);
        candidate.active_tab = Some(tabs[active].0);
        candidate.active_view = candidate.panes.get(tabs[active].1).map(|pane| pane.view);
        if candidate.validate_layout().is_err() {
            return;
        }
        self.state = candidate;
        self.ui.dashboard = false;
        for (canonical, _) in restore.loaded.values() {
            self.record_recent_file(canonical.clone());
        }
    }

    fn fail_session_restore(&mut self) {
        if let Some(restore) = &mut self.session_restore {
            restore.failed = true;
            restore.waiting = restore.waiting.saturating_sub(1);
            if restore.waiting == 0 {
                self.session_restore = None
            }
        }
    }

    fn accept_session_restore_read(
        &mut self,
        requested: PathBuf,
        canonical: PathBuf,
        text: TextBuffer,
    ) {
        let mut finish = false;
        if let Some(restore) = &mut self.session_restore {
            restore.loaded.insert(requested, (canonical, text));
            restore.waiting = restore.waiting.saturating_sub(1);
            finish = restore.waiting == 0;
        }
        if finish {
            self.finish_session_restore();
        }
    }

    fn record_recent_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|candidate| candidate != &path);
        self.recent_files.insert(0, path);
        self.recent_files
            .truncate(self.configuration.settings.persistence.recent_files);
    }

    fn syntax_language_label(&self, language: SyntaxLanguage) -> String {
        self.message(
            match language {
                SyntaxLanguage::Rust => "syntax_language_rust",
                SyntaxLanguage::TypeScript => "syntax_language_typescript",
                SyntaxLanguage::Tsx => "syntax_language_tsx",
                SyntaxLanguage::JavaScript => "syntax_language_javascript",
                SyntaxLanguage::Go => "syntax_language_go",
                SyntaxLanguage::Json => "syntax_language_json",
                SyntaxLanguage::Toml => "syntax_language_toml",
                SyntaxLanguage::Markdown => "syntax_language_markdown",
            },
            &[],
        )
    }
    fn syntax_support_label(&self, support: SyntaxSupport) -> String {
        self.message(
            match support {
                SyntaxSupport::Full => "syntax_support_full",
                SyntaxSupport::Partial => "syntax_support_partial",
                SyntaxSupport::Unsupported => "syntax_support_unsupported",
            },
            &[],
        )
    }
    fn apply_syntax_indent_line(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let Some(language) = buffer.path.as_deref().and_then(language_for_path) else {
            self.show_error(self.message("syntax_indent_unavailable", &[]));
            return;
        };
        let revision = buffer.text.revision();
        let line = match buffer.text.byte_to_line(cursor) {
            Ok(line) => line,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let Some(result) = self.syntax_accepted.get(&buffer_id) else {
            self.show_error(self.message("syntax_indent_pending", &[]));
            return;
        };
        if result.language != language || result.revision != revision.0 {
            self.show_error(self.message("syntax_indent_pending", &[]));
            return;
        }
        if result.capabilities.indents == SyntaxSupport::Unsupported {
            self.show_error(self.message(
                "syntax_indent_unsupported",
                &[("language", self.syntax_language_label(language))],
            ));
            return;
        }
        if result.indents_truncated {
            self.show_error(self.message("syntax_indent_truncated", &[]));
            return;
        }
        let levels = result
            .indents
            .iter()
            .filter(|range| line > range.start_row && line < range.end_row)
            .count();
        let desired = if self.state.policy.expand_tabs {
            " ".repeat(levels.saturating_mul(self.state.policy.indent_width))
        } else {
            "\t".repeat(levels)
        };
        let text = match buffer.text.line(line) {
            Ok(text) => text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let whitespace = text
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        if text.get(..whitespace) == Some(desired.as_str()) {
            self.ui.notification = Some(self.message("syntax_indent_unchanged", &[]));
            self.ui.notification_error = false;
            return;
        }
        let start = match buffer.text.line_to_byte(line) {
            Ok(start) => start,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let end = ByteOffset(start.get().saturating_add(whitespace));
        let edits = TextEditSet::new(
            revision,
            vec![TextEdit::new(
                ByteRange::new(start, end).expect("ordered indentation"),
                desired,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.ui.notification = Some(self.message("syntax_indent_applied", &[]));
            self.ui.notification_error = false;
        }
    }

    fn accepts_syntax_event(
        &self,
        buffer: crate::editing::BufferId,
        revision: u64,
        generation: u64,
        language: SyntaxLanguage,
    ) -> bool {
        self.syntax_submitted.get(&buffer)
            == Some(&(BufferRevision(revision), generation, language))
            && self.state.buffers.get(buffer).is_some_and(|state| {
                state.text.revision().0 == revision
                    && state.path.as_deref().and_then(language_for_path) == Some(language)
            })
    }
    fn process_syntax(&mut self) {
        if !self.configuration.settings.syntax.enabled {
            for document in self.syntax_documents.values().copied() {
                self.syntax.cancel(document);
            }
            self.syntax_documents.clear();
            self.syntax_submitted.clear();
            self.syntax_accepted.clear();
            self.syntax_failures.clear();
            self.syntax_due.clear();
            self.ui.syntax_spans.clear();
            self.ui.syntax_folds.clear();
            return;
        }
        let live = self.state.buffers.keys().collect::<Vec<_>>();
        self.syntax_documents.retain(|buffer, document| {
            let keep = live.contains(buffer);
            if !keep {
                self.syntax.cancel(*document);
            }
            keep
        });
        self.syntax_submitted
            .retain(|buffer, _| live.contains(buffer));
        self.syntax_accepted.retain(|buffer, result| {
            live.contains(buffer)
                && self.state.buffers.get(*buffer).is_some_and(|state| {
                    state.path.as_deref().and_then(language_for_path) == Some(result.language)
                        && state.text.revision().0 == result.revision
                })
        });
        self.syntax_failures.retain(|buffer, failure| {
            live.contains(buffer)
                && self.state.buffers.get(*buffer).is_some_and(|state| {
                    state.text.revision() == failure.revision
                        && state.path.as_deref().and_then(language_for_path)
                            == Some(failure.language)
                })
                && self.syntax_submitted.get(buffer)
                    == Some(&(failure.revision, failure.generation, failure.language))
        });
        self.syntax_due.retain(|buffer, _| live.contains(buffer));
        for buffer in &live {
            if self
                .state
                .buffers
                .get(*buffer)
                .and_then(|state| state.path.as_deref())
                .and_then(language_for_path)
                .is_none()
            {
                if let Some(document) = self.syntax_documents.remove(buffer) {
                    self.syntax.cancel(document);
                }
                self.syntax_submitted.remove(buffer);
                self.syntax_accepted.remove(buffer);
                self.syntax_failures.remove(buffer);
                self.syntax_due.remove(buffer);
            }
        }
        let now = Instant::now();
        let candidates = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                let path = buffer.path.as_ref()?;
                let language = language_for_path(path)?;
                let large = self.effective_large_file(id);
                Some((id, buffer.text.revision(), language, large))
            })
            .collect::<Vec<_>>();
        for (buffer, revision, language, large) in candidates {
            if self
                .syntax_submitted
                .get(&buffer)
                .is_some_and(|(submitted, _, previous)| {
                    *submitted != revision || *previous != language
                })
            {
                self.syntax_failures.remove(&buffer);
                self.syntax_accepted.remove(&buffer);
            }
            if large && self.configuration.settings.editor.large_file.disable_syntax {
                if let Some(document) = self.syntax_documents.remove(&buffer) {
                    self.syntax.cancel(document);
                }
                self.syntax_submitted.remove(&buffer);
                self.syntax_accepted.remove(&buffer);
                self.syntax_failures.remove(&buffer);
                self.syntax_due.remove(&buffer);
                continue;
            }
            if self.syntax_submitted.get(&buffer).is_some_and(
                |(submitted, _, submitted_language)| {
                    *submitted == revision && *submitted_language == language
                },
            ) {
                continue;
            }
            let due = self.syntax_due.entry(buffer).or_insert((
                revision,
                now + Duration::from_millis(self.configuration.settings.syntax.debounce_ms),
            ));
            if due.0 != revision {
                *due = (
                    revision,
                    now + Duration::from_millis(self.configuration.settings.syntax.debounce_ms),
                );
                continue;
            }
            if now < due.1 {
                continue;
            }
            let document = *self.syntax_documents.entry(buffer).or_insert_with(|| {
                let id = SyntaxDocumentId(self.next_syntax_document);
                self.next_syntax_document = self.next_syntax_document.saturating_add(1);
                id
            });
            let generation = self.next_syntax_generation;
            self.next_syntax_generation = self.next_syntax_generation.saturating_add(1);
            let configured = &self.configuration.settings.syntax;
            let policy = SyntaxPolicy {
                maximum_source_bytes: configured.maximum_source_bytes,
                maximum_spans: configured.maximum_spans,
                maximum_folds: configured.maximum_folds,
                maximum_indent_rows: configured.maximum_indent_rows,
            };
            let Some(source) = self
                .state
                .buffers
                .get(buffer)
                .map(|state| state.text.to_string())
            else {
                continue;
            };
            if self
                .syntax
                .submit(document, generation, revision.0, language, source, policy)
                .is_ok()
            {
                self.syntax_submitted
                    .insert(buffer, (revision, generation, language));
                self.syntax_due.remove(&buffer);
            }
        }
        while let Ok(Some(event)) = self.syntax.try_event() {
            match event {
                SyntaxEvent::Parsed(result) => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, document)| **document == result.document)
                    {
                        if self.accepts_syntax_event(
                            buffer,
                            result.revision,
                            result.generation,
                            result.language,
                        ) {
                            self.syntax_failures.remove(&buffer);
                            self.syntax_accepted.insert(buffer, result);
                        }
                    }
                }
                SyntaxEvent::Failed {
                    document,
                    generation,
                    revision,
                    language,
                    diagnostic,
                } => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, candidate)| **candidate == document)
                        && self.accepts_syntax_event(buffer, revision, generation, language)
                    {
                        self.syntax_accepted.remove(&buffer);
                        self.syntax_failures.insert(
                            buffer,
                            SyntaxFailure {
                                revision: BufferRevision(revision),
                                generation,
                                language,
                                diagnostic,
                            },
                        );
                    }
                }
                SyntaxEvent::Skipped {
                    document,
                    generation,
                    revision,
                    language,
                } => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, candidate)| **candidate == document)
                        && self.accepts_syntax_event(buffer, revision, generation, language)
                    {
                        self.syntax_accepted.remove(&buffer);
                        self.syntax_failures.insert(
                            buffer,
                            SyntaxFailure {
                                revision: BufferRevision(revision),
                                generation,
                                language,
                                diagnostic: self.message("syntax_source_too_large", &[]),
                            },
                        );
                    }
                }
            }
        }

        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax_accepted.get(&view.buffer) {
                for fold in &result.folds {
                    syntax_folds.push(UiFold {
                        view: view_id,
                        start_line: fold.start_row,
                        end_line: fold.end_row,
                        collapsed: collapsed.contains(&(view_id, fold.start_row, fold.end_row)),
                    });
                }
            }
        }
        self.ui.syntax_folds = syntax_folds;
        self.ui.syntax_spans = self
            .syntax_accepted
            .iter()
            .flat_map(|(buffer, result)| {
                result.spans.iter().filter_map(|span| {
                    Some(UiSyntaxSpan {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        role: span.role,
                    })
                })
            })
            .collect();
    }

    fn add_workspace_root(&mut self, path: PathBuf) {
        if self.pending_root_operation.is_some() {
            self.show_error(self.message("workspace_root_operation_busy", &[]));
            return;
        }
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.trust_service.inspect_root(requested_path.clone()) {
            Ok(generation) => {
                self.pending_root_operation = Some(PendingRootOperation {
                    generation,
                    root_revision: self.workspace.root_revision(),
                    requested_path,
                });
                self.ui.notification = Some(self.message("workspace_root_loading", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn root_removal_blocker(&self, root: crate::workspace::RootId) -> Option<String> {
        let root = self
            .workspace
            .roots()
            .iter()
            .find(|candidate| candidate.id == root)?;
        if self.state.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root.identity.canonical_path))
        }) {
            return Some(self.message("workspace_root_dirty_blocker", &[]));
        }
        let busy = self.pending_root_operation.is_some()
            || self.active_lsp_server.is_some()
            || self
                .lsp_sessions
                .values()
                .any(|state| state.restart_due.is_some())
            || self.dap.is_running()
            || self
                .tools
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing))
            || self.workspace_search_generation.is_some()
            || self.pending_workspace_proposal.is_some()
            || self.workspace_proposal.is_some()
            || !self.pending_vfs.is_empty()
            || self.trust_persist_generation.is_some()
            || self.execution_validation.is_some()
            || !self.startup_reads.is_empty()
            || self.startup_reads_pending > 0;
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }

    fn invalidate_root_scoped_operations(&mut self) {
        for terminal in self.terminals.keys().copied().collect::<Vec<_>>() {
            let _ = self.ptys.close(terminal);
        }
        self.active_terminal = None;
        self.ui.terminal_panel = None;
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .lsp_sessions
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.lsp.stop(&key);
            self.clear_lsp_server_ownership(&key);
            self.lsp_sessions.remove(&key);
            self.lsp_session_synced
                .retain(|(server, _), _| server != &key);
            self.lsp_synced_revisions.clear();
            if self.active_lsp_server.as_deref() == Some(&key) {
                self.active_lsp_server = None;
                self.lsp_capabilities = None;
                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.pending_execution_preflight.clear();
        self.registry_intents.clear();
        self.pending_registry_artifact = None;
        for (plugin, (load, token)) in self.package_prepared.drain().collect::<Vec<_>>() {
            let _ = self.plugins.unload_generation(&plugin, load);
            self.commands.unregister_plugin(&plugin);
            let _ = self.packages.request(PackageOperation::Abort { token });
        }
        for (plugin, load) in self.package_commits.values().cloned().collect::<Vec<_>>() {
            let _ = self.plugins.unload_generation(&plugin, load);
            self.commands.unregister_plugin(&plugin);
        }
        let _ = self.dap.stop();
        self.dap_responses.clear();
        self.dap_console.clear();
        self.dap_terminal_responses.clear();
        self.active_debug_terminal_profile = None;
        self.active_debug_configuration = None;
        self.dap_breakpoints.clear();
        self.dap_watches.clear();
        self.snippet_session = None;
        for name in self.task_status.keys() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.task_status.clear();
        self.pending_linter_builds.clear();
        self.task_output.clear();
        self.task_output_rows.clear();
        self.test_results.clear();
        self.task_truncated.clear();
        for job in self.job_tasks.keys().copied().collect::<Vec<_>>() {
            let _ = self.jobs.cancel(job);
        }
        self.workspace_search.cancel();
        self.workspace_search_generation = None;
        self.workspace_search_intent = None;
        self.workspace_search_matches.clear();
        self.workspace_search_root_revision = None;
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            self.reject_workspace_proposal();
        }
        self.workspace_proposal_root_revision = None;
        self.workspace_resource_intents.clear();
        self.accepted_resource_intents.clear();
        self.resource_apply_pending.clear();
        self.explorer_pending = None;
        self.explorer_preview_path = None;
        self.explorer_preview_lines.clear();
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.ui.explorer_focused = false;
        self.close_picker();
    }

    fn remove_workspace_root(&mut self, id: crate::workspace::RootId) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                self.invalidate_root_scoped_operations();
                self.ui.notification = Some(self.message(
                    "workspace_root_removed",
                    &[("path", root.identity.canonical_path.display().to_string())],
                ));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn activate_workspace_root(&mut self, id: crate::workspace::RootId) {
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                let path = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.display().to_string())
                    .unwrap_or_default();
                self.ui.notification =
                    Some(self.message("workspace_root_activated", &[("path", path)]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn update_live_crash_snapshot(&mut self) {
        let now = Instant::now();
        if now < self.next_crash_snapshot {
            return;
        }
        self.next_crash_snapshot = now + Duration::from_secs(1);
        let timestamp_ms = crate::clock::system_time()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let _ = self.crash_snapshot.submit(LiveCrashSnapshot {
            timestamp_ms,
            roots: self.workspace.roots().len(),
            buffers: self.state.buffers.len(),
            dirty_buffers: self
                .state
                .buffers
                .values()
                .filter(|buffer| buffer.dirty)
                .count(),
            jobs: self.job_tasks.len(),
            ptys: self.terminals.len(),
            lsp_sessions: self.lsp_sessions.len(),
            dap_running: self.dap.is_running(),
            pending_effects: self
                .pending_vfs
                .len()
                .saturating_add(self.pending_execution_preflight.len())
                .saturating_add(self.pending_linter_builds.len()),
            plugins_failed: self
                .plugins
                .statuses
                .values()
                .filter(|status| matches!(status, PluginStatus::Failed(_)))
                .count(),
            safe_mode: self.safe_mode,
        });
    }

    fn defer_trusted_execution(&mut self, action: DeferredExecution) {
        if self.safe_mode {
            self.show_error(self.message("safe_mode_blocked", &[]));
            return;
        }
        if self.execution_validation.is_some() {
            self.show_error(self.message("workspace_execution_validation_busy", &[]));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
        {
            self.show_error(error);
            return;
        }
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        let roots = self.workspace.roots().to_vec();
        let mut generations = Vec::new();
        for root in &roots {
            match self
                .trust_service
                .inspect_root(root.identity.canonical_path.clone())
            {
                Ok(generation) => generations.push((generation, root.id, root.identity.clone())),
                Err(error) => {
                    self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    self.show_error(self.message(
                        "workspace_execution_validation_failed",
                        &[("diagnostic", error.to_string())],
                    ));
                    return;
                }
            }
        }
        self.execution_validation_generations.clear();
        for (generation, root, identity) in generations {
            self.execution_validation_generations
                .insert(generation, (root, identity));
        }
        self.execution_validation = Some(PendingExecutionValidation {
            action,
            root_revision: self.workspace.root_revision(),
            failed: false,
        });
        self.ui.notification = Some(self.message("workspace_execution_validating", &[]));
        self.ui.notification_error = false;
    }

    fn finish_execution_validation_if_ready(&mut self) {
        if !self.execution_validation_generations.is_empty() {
            return;
        }
        let Some(pending) = self.execution_validation.take() else {
            return;
        };
        if pending.failed || pending.root_revision != self.workspace.root_revision() {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message(
                "workspace_execution_validation_failed",
                &[("diagnostic", "workspace root identity changed".into())],
            ));
            return;
        }
        self.execution_revalidated = true;
        match pending.action {
            DeferredExecution::InstallTool(id) => self.activate_tool_by_id(&id),
            DeferredExecution::StartLsp(requested) => self.start_lsp(requested),
            DeferredExecution::RestartLsp(key) => self.restart_lsp_session(key),
            DeferredExecution::StartDap { requested, program } => {
                self.start_dap(requested, program);
            }
            DeferredExecution::StartDapConfiguration(name) => self.start_dap_configuration(&name),
            DeferredExecution::InstallRegistry {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                true,
            ),
            DeferredExecution::PrepareRegistryArtifact => {
                if let Some((metadata, component)) = self.pending_registry_artifact.take()
                    && let Err(error) = self.packages.request(PackageOperation::PrepareRegistry {
                        metadata,
                        component,
                        limits: self.plugin_limits(),
                    })
                {
                    self.show_error(error)
                }
            }
            DeferredExecution::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => self.install_package_after_validation(
                package_manifest,
                plugin_manifest,
                component,
                update,
            ),
            DeferredExecution::RunTask(name) => self.start_task_after_validation(&name),
            DeferredExecution::OpenTerminal(profile) => {
                self.open_terminal_after_validation_with_response(&profile, None)
            }
            DeferredExecution::OpenDapTerminal {
                profile,
                request_seq,
            } => self.open_terminal_after_validation_with_response(&profile, Some(request_seq)),
            DeferredExecution::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            DeferredExecution::RunLinter { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Linter { matcher })
            }
            DeferredExecution::RunFormatter {
                name,
                buffer,
                revision,
                source,
            } => self.start_task_plan(
                &name,
                AdapterContext::Formatter {
                    buffer,
                    revision,
                    source,
                },
            ),
        }
        self.execution_revalidated = false;
    }

    fn reconcile_workspace_trust(&mut self) {
        let trusted = self.trust_database_ready
            && self.trust_identity_ready
            && self.workspace.roots().iter().all(|root| {
                root.identity.filesystem_identity.is_some()
                    && self.trust_store.is_trusted(&root.identity)
            });
        self.workspace.set_trust(if trusted {
            WorkspaceTrust::Trusted
        } else {
            WorkspaceTrust::Untrusted
        });
    }

    fn process_trust_events(&mut self) {
        loop {
            let event = match self.trust_service.try_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    self.execution_validation = None;
                    self.execution_validation_generations.clear();
                    self.show_error(self.message(
                        "workspace_trust_state_failed",
                        &[("diagnostic", error.to_string())],
                    ));
                    break;
                }
            };
            match event {
                TrustEvent::Loaded { generation, result }
                    if self.trust_load_generation == Some(generation) =>
                {
                    self.trust_load_generation = None;
                    self.trust_database_ready = true;
                    match result {
                        Ok(database) => self.trust_store.replace(database.identities),
                        Err(error) => {
                            self.trust_store.replace(Vec::new());
                            self.show_error(self.message(
                                "workspace_trust_state_failed",
                                &[("diagnostic", error.to_string())],
                            ));
                        }
                    }
                    self.reconcile_workspace_trust();
                }
                TrustEvent::RootInspected { generation, result }
                    if self.trust_identity_generation == Some(generation) =>
                {
                    self.trust_identity_generation = None;
                    match result {
                        Ok(identity) => {
                            let root = self.workspace.active_root();
                            if let Some(root) = root {
                                match self.workspace.replace_root_identity(root, identity) {
                                    Ok(()) => self.trust_identity_ready = true,
                                    Err(error) => self.show_error(error),
                                }
                            }
                        }
                        Err(error) => self.show_error(error),
                    }
                    self.reconcile_workspace_trust();
                }
                TrustEvent::RootInspected { generation, result }
                    if self
                        .pending_root_operation
                        .as_ref()
                        .is_some_and(|operation| operation.generation == generation) =>
                {
                    let operation = self
                        .pending_root_operation
                        .take()
                        .expect("matched root operation");
                    if operation.root_revision != self.workspace.root_revision() {
                        self.show_error(self.message("workspace_root_stale", &[]));
                        continue;
                    }
                    match result {
                        Ok(identity) => {
                            let name = identity.canonical_path.file_name().map_or_else(
                                || identity.canonical_path.display().to_string(),
                                |name| name.to_string_lossy().into_owned(),
                            );
                            match self.workspace.add_root(name, identity.clone()) {
                                Ok(_) => {
                                    self.invalidate_root_scoped_operations();
                                    self.ui.notification = Some(self.message(
                                        "workspace_root_added",
                                        &[("path", identity.canonical_path.display().to_string())],
                                    ));
                                    self.ui.notification_error = false;
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        Err(error) => self.show_error(self.message(
                            "workspace_root_add_failed",
                            &[
                                ("path", operation.requested_path.display().to_string()),
                                ("diagnostic", error.to_string()),
                            ],
                        )),
                    }
                }
                TrustEvent::RootInspected { generation, result }
                    if self
                        .execution_validation_generations
                        .contains_key(&generation) =>
                {
                    let (_, expected) = self
                        .execution_validation_generations
                        .remove(&generation)
                        .expect("checked execution validation generation");
                    let valid = result.as_ref().is_ok_and(|identity| identity == &expected)
                        && self.trust_store.is_trusted(&expected);
                    if !valid {
                        if let Some(pending) = &mut self.execution_validation {
                            pending.failed = true;
                        }
                        self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    }
                    self.finish_execution_validation_if_ready();
                }
                TrustEvent::Persisted { generation, result }
                    if self.trust_persist_generation == Some(generation) =>
                {
                    self.trust_persist_generation = None;
                    let target = self.trust_persist_target.take();
                    match result {
                        Ok(()) => {
                            self.reconcile_workspace_trust();
                            let key = if target == Some(WorkspaceTrust::Trusted) {
                                "workspace_trusted"
                            } else {
                                "workspace_untrusted"
                            };
                            self.ui.notification = Some(self.message(key, &[]));
                            self.ui.notification_error = false;
                        }
                        Err(error) => {
                            if target == Some(WorkspaceTrust::Trusted) {
                                for root in self.workspace.roots() {
                                    self.trust_store.revoke(&root.identity);
                                }
                            }
                            self.workspace.set_trust(WorkspaceTrust::Untrusted);
                            self.show_error(self.message(
                                "workspace_trust_state_failed",
                                &[("diagnostic", error.to_string())],
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn persist_workspace_trust(&mut self, target: WorkspaceTrust) {
        match self
            .trust_service
            .persist(self.trust_state_path.clone(), self.trust_store.identities())
        {
            Ok(generation) => {
                self.trust_persist_generation = Some(generation);
                self.trust_persist_target = Some(target);
                self.ui.notification = Some(self.message("workspace_trust_saving", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => {
                self.workspace.set_trust(WorkspaceTrust::Untrusted);
                self.show_error(self.message(
                    "workspace_trust_state_failed",
                    &[("diagnostic", error.to_string())],
                ));
            }
        }
    }

    fn request_workspace_trust(&mut self) {
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        self.open_prompt(PromptKind::WorkspaceTrust, String::new());
        let confirmation = self.message(
            "workspace_trust_confirmation",
            &[(
                "phrase",
                self.configuration
                    .settings
                    .workspace
                    .trust_confirmation_phrase
                    .clone(),
            )],
        );
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.error = Some(confirmation);
        }
    }

    fn confirm_workspace_trust(&mut self, phrase: String) {
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        if phrase
            != self
                .configuration
                .settings
                .workspace
                .trust_confirmation_phrase
        {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message("workspace_trust_confirmation_wrong", &[]));
            return;
        }
        for root in self.workspace.roots() {
            if root.identity.filesystem_identity.is_none() {
                self.show_error(self.message("workspace_trust_not_ready", &[]));
                return;
            }
            self.trust_store.trust(root.identity.clone());
        }
        self.persist_workspace_trust(WorkspaceTrust::Trusted);
    }

    fn revoke_workspace_trust(&mut self) {
        self.workspace.set_trust(WorkspaceTrust::Untrusted);
        for root in self.workspace.roots() {
            self.trust_store.revoke(&root.identity);
        }
        if self.trust_database_ready {
            self.persist_workspace_trust(WorkspaceTrust::Untrusted);
        } else {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
        }
    }

    fn lsp_position_encoding(&self) -> PositionEncoding {
        self.lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            })
    }

    fn start_workspace_search(&mut self, query: String, replacement: Option<String>) {
        let configured = &self.configuration.settings.workspace.search;
        let policy = WorkspaceSearchPolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            query,
            regex: configured.regex,
            smart_case: configured.smart_case,
            whole_word: configured.whole_word,
            include_globs: configured.include_globs.clone(),
            exclude_globs: configured.exclude_globs.clone(),
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_results: configured.maximum_results,
            maximum_file_bytes: configured.maximum_file_bytes,
            maximum_total_bytes: configured.maximum_total_bytes,
            maximum_preview_bytes: configured.maximum_preview_bytes,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.workspace_search.start(policy) {
            Ok(generation) => {
                self.workspace_search_generation = Some(generation);
                self.workspace_search_root_revision = Some(self.workspace.root_revision());
                self.workspace_search_matches.clear();
                self.workspace_search_intent = Some(
                    replacement.map_or(WorkspaceSearchIntent::Find, WorkspaceSearchIntent::Replace),
                );
                self.close_picker();
                self.ui.notification = Some(self.message("workspace_search_started", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(self.message(
                "workspace_search_failed",
                &[("diagnostic", error.to_string())],
            )),
        }
    }

    fn process_workspace_search_events(&mut self) {
        while let Some(event) = self.workspace_search.try_event() {
            match event {
                WorkspaceSearchEvent::Batch {
                    generation,
                    matches,
                } if self.workspace_search_generation == Some(generation) => {
                    self.workspace_search_matches.extend(matches);
                }
                WorkspaceSearchEvent::Complete {
                    generation,
                    summary,
                } if self.workspace_search_generation == Some(generation) => {
                    self.workspace_search_generation = None;
                    let intent = self.workspace_search_intent.take();
                    match intent {
                        Some(WorkspaceSearchIntent::Find) => {
                            if self.workspace_search_matches.is_empty() {
                                self.show_error(self.message("workspace_search_empty", &[]));
                            } else {
                                self.open_workspace_search_picker();
                                self.ui.notification = Some(self.message(
                                    "workspace_search_complete",
                                    &[
                                        ("count", self.workspace_search_matches.len().to_string()),
                                        ("files", summary.files_searched.to_string()),
                                    ],
                                ));
                                self.ui.notification_error = false;
                            }
                        }
                        Some(WorkspaceSearchIntent::Replace(replacement)) => {
                            if self.workspace_search_matches.is_empty() {
                                self.show_error(self.message("workspace_search_empty", &[]));
                            } else {
                                self.begin_workspace_search_replacement(replacement);
                            }
                        }
                        None => {}
                    }
                }
                WorkspaceSearchEvent::Failed { generation, error }
                    if self.workspace_search_generation == Some(generation) =>
                {
                    self.workspace_search_generation = None;
                    self.workspace_search_intent = None;
                    self.workspace_search_matches.clear();
                    self.show_error(
                        self.message("workspace_search_failed", &[("diagnostic", error)]),
                    );
                }
                WorkspaceSearchEvent::Cancelled { generation }
                    if self.workspace_search_generation == Some(generation) =>
                {
                    self.workspace_search_generation = None;
                    self.workspace_search_intent = None;
                    self.workspace_search_matches.clear();
                }
                _ => {}
            }
        }
    }

    fn open_workspace_search_picker(&mut self) {
        let items = self
            .workspace_search_matches
            .iter()
            .cloned()
            .map(|found| {
                let item = PickerItem {
                    id: PickerItemId(found.id.clone()),
                    label: self.message(
                        "workspace_search_row",
                        &[
                            ("path", found.relative_path.display().to_string()),
                            ("line", found.line.saturating_add(1).to_string()),
                            ("column", found.column.saturating_add(1).to_string()),
                        ],
                    ),
                    detail: found.line_text.trim().to_owned(),
                    source: found
                        .root
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                    action: PickerAction::None,
                };
                (item, PickerPayload::WorkspaceSearch(found))
            })
            .collect();
        self.install_picker(self.message("workspace_search_title", &[]), items);
    }

    fn begin_workspace_search_replacement(&mut self, replacement: String) {
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let mut grouped = BTreeMap::<PathBuf, (Vec<u8>, Vec<TextEdit>)>::new();
        for found in &self.workspace_search_matches {
            let entry = grouped
                .entry(found.canonical_path.clone())
                .or_insert_with(|| (found.snapshot.to_vec(), Vec::new()));
            if entry.0.as_slice() != found.snapshot.as_ref() {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            }
            let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            else {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            };
            entry.1.push(TextEdit::new(range, replacement.clone()));
        }
        let mut snapshots = Vec::with_capacity(grouped.len());
        let mut drafts = Vec::with_capacity(grouped.len());
        for (path, (searched_bytes, edits)) in grouped {
            let revision = if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                if buffer.text.to_bytes() != searched_bytes {
                    self.show_error(self.message("workspace_search_stale_result", &[]));
                    return;
                }
                Some(buffer.text.revision())
            } else {
                None
            };
            snapshots.push(FileSnapshot {
                canonical_path: path.clone(),
                revision,
                bytes: searched_bytes,
            });
            drafts.push(WorkspaceEditDraft {
                canonical_path: path,
                expected_revision: revision,
                edits,
            });
        }
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.saturating_add(1);
        match WorkspaceEditProposal::build(
            id,
            self.message("workspace_replace_label", &[]),
            &self.workspace,
            drafts,
            snapshots,
        ) {
            Ok(proposal) => {
                self.workspace_proposal_response = None;
                self.workspace_proposal_root_revision = Some(self.workspace.root_revision());
                self.workspace_proposal = Some(proposal);
                self.show_workspace_proposal();
            }
            Err(error) => self.show_error(self.message(
                "workspace_edit_failed",
                &[("diagnostic", error.to_string())],
            )),
        }
    }

    fn install_picker(&mut self, title: String, items: Vec<(PickerItem, PickerPayload)>) {
        self.completion_generation = self.completion_generation.saturating_add(1);
        let generation = self.completion_generation;
        let mut picker = PickerState::new(
            generation,
            title,
            self.configuration.settings.workspace.maximum_picker_items,
        );
        self.picker_payloads.clear();
        let rows = items
            .into_iter()
            .map(|(item, payload)| {
                self.picker_payloads.insert(item.id.clone(), payload);
                item
            })
            .collect::<Vec<_>>();
        picker.append(generation, rows, true);
        self.picker = Some(picker);
        self.sync_picker_ui();
    }

    fn open_lsp_locations_picker(
        &mut self,
        server: &str,
        locations: Vec<crate::languages::LspLocation>,
    ) {
        let items = locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| {
                let path = path_from_file_uri(&location.uri)
                    .map_or_else(|| location.uri.clone(), |path| path.display().to_string());
                let id = PickerItemId(format!("location:{index}:{}", location.uri));
                (
                    PickerItem {
                        id,
                        label: path,
                        detail: format!(
                            "{}:{}",
                            location.range.start.line + 1,
                            location.range.start.character + 1
                        ),
                        source: server.into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::LspLocation(location),
                )
            })
            .collect();
        self.install_picker(self.message("picker_references_title", &[]), items);
    }

    fn open_lsp_symbols_picker(&mut self, server: &str, symbols: Vec<crate::languages::LspSymbol>) {
        let items = symbols
            .into_iter()
            .enumerate()
            .map(|(index, symbol)| {
                let id = PickerItemId(format!("symbol:{index}:{}", symbol.name));
                let payload = symbol.location.map_or_else(
                    || PickerPayload::Command(String::new()),
                    PickerPayload::LspLocation,
                );
                (
                    PickerItem {
                        id,
                        label: symbol.name,
                        detail: symbol.kind.map_or_else(String::new, |kind| {
                            self.message("picker_symbol_kind", &[("kind", kind.to_string())])
                        }),
                        source: server.into(),
                        action: PickerAction::None,
                    },
                    payload,
                )
            })
            .collect();
        self.install_picker(self.message("picker_symbols_title", &[]), items);
    }

    fn open_lsp_code_actions_picker(
        &mut self,
        server: &str,
        actions: Vec<crate::languages::LspCodeAction>,
    ) {
        let items = actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| {
                let id = PickerItemId(format!("code-action:{index}:{}", action.title));
                (
                    PickerItem {
                        id,
                        label: action.title.clone(),
                        detail: action.kind.clone().unwrap_or_default(),
                        source: server.into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::CodeAction(action),
                )
            })
            .collect();
        self.install_picker(self.message("picker_code_actions_title", &[]), items);
    }

    fn open_dap_picker(&mut self, kind: &str, rows: Vec<(String, String, DapControl)>) {
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, (label, detail, control))| {
                (
                    PickerItem {
                        id: PickerItemId(format!("dap:{kind}:{index}")),
                        label,
                        detail,
                        source: "dap".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DapControl(control),
                )
            })
            .collect();
        self.install_picker(
            self.message("dap_picker_title", &[("kind", kind.into())]),
            items,
        );
    }

    fn open_dap_frames_picker(&mut self, rows: Vec<DapStackFrame>) {
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                (
                    PickerItem {
                        id: PickerItemId(format!("dap:frame:{index}:{}", row.id)),
                        label: row.name.clone(),
                        detail: row
                            .path
                            .as_ref()
                            .map(|path| format!("{}:{}:{}", path.display(), row.line, row.column))
                            .unwrap_or_default(),
                        source: "dap".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::DapFrame(row),
                )
            })
            .collect();
        self.install_picker(
            self.message("dap_picker_title", &[("kind", "frames".into())]),
            items,
        );
    }

    fn open_builtin_picker(&mut self, kind: PickerKind) {
        match kind {
            PickerKind::Diagnostics => {
                let items = self
                    .diagnostics
                    .problems(None, None)
                    .into_iter()
                    .enumerate()
                    .map(|(index, problem)| {
                        let id = PickerItemId(format!("diagnostic:{index}:{}", problem.uri));
                        let path = path_from_file_uri(&problem.uri)
                            .map_or_else(|| problem.uri.clone(), |path| path.display().to_string());
                        (
                            PickerItem {
                                id,
                                label: problem.message.clone(),
                                detail: path,
                                source: problem.source,
                                action: PickerAction::None,
                            },
                            PickerPayload::Diagnostic {
                                uri: problem.uri,
                                range: problem.range,
                            },
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_diagnostics_title", &[]), items);
            }
            PickerKind::Buffers => {
                let items = self
                    .state
                    .buffers
                    .iter()
                    .enumerate()
                    .map(|(index, (buffer, state))| {
                        let id = PickerItemId(format!("buffer:{index}"));
                        (
                            PickerItem {
                                id,
                                label: state.path.as_ref().map_or_else(
                                    || {
                                        self.configuration
                                            .settings
                                            .ui
                                            .status_line
                                            .scratch_label
                                            .clone()
                                    },
                                    |path| path.display().to_string(),
                                ),
                                detail: if state.dirty {
                                    self.configuration
                                        .settings
                                        .ui
                                        .status_line
                                        .dirty_marker
                                        .clone()
                                } else {
                                    String::new()
                                },
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Buffer(buffer),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_buffers_title", &[]), items);
            }
            PickerKind::Roots => {
                let active = self.workspace.active_root();
                let trusted = self.workspace.trust() == WorkspaceTrust::Trusted;
                let items = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| {
                        let state = if active == Some(root.id) {
                            self.message("workspace_root_active", &[])
                        } else {
                            self.message("workspace_root_inactive", &[])
                        };
                        let trust = if trusted {
                            self.message("workspace_trust_trusted", &[])
                        } else {
                            self.message("workspace_trust_untrusted", &[])
                        };
                        (
                            PickerItem {
                                id: PickerItemId(format!("workspace-root:{}", root.id.0)),
                                label: self.message(
                                    "workspace_root_row",
                                    &[
                                        ("id", root.id.0.to_string()),
                                        ("name", root.name.clone()),
                                        ("state", state),
                                        ("trust", trust),
                                    ],
                                ),
                                detail: root.identity.canonical_path.display().to_string(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::WorkspaceRoot(root.id),
                        )
                    })
                    .collect();
                self.install_picker(self.message("workspace_root_picker_title", &[]), items);
            }
            PickerKind::Recents => {
                let items = self
                    .recent_files
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, path)| {
                        let location = PersistedLocation {
                            path: path.clone(),
                            byte: 0,
                            anchor: 0,
                            scroll_line: 0,
                        };
                        (
                            PickerItem {
                                id: PickerItemId(format!("recent:{index}")),
                                label: path.display().to_string(),
                                detail: String::new(),
                                source: self.message("navigation_recent_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_recents_title", &[]), items);
            }
            PickerKind::Marks => {
                let items = self
                    .marks
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, mark)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("mark:{index}:{}", mark.name)),
                                label: mark.name,
                                detail: format!(
                                    "{}:{}",
                                    mark.location.path.display(),
                                    mark.location.byte
                                ),
                                source: self.message("navigation_mark_source", &[]),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(mark.location),
                        )
                    })
                    .collect();
                self.install_picker(self.message("navigation_marks_title", &[]), items);
            }
            PickerKind::Jumps | PickerKind::Changes => {
                let (source, title, rows) = if matches!(kind, PickerKind::Jumps) {
                    (
                        self.message("navigation_jump_source", &[]),
                        self.message("navigation_jumps_title", &[]),
                        self.jump_list.clone(),
                    )
                } else {
                    (
                        self.message("navigation_change_source", &[]),
                        self.message("navigation_changes_title", &[]),
                        self.change_list.clone(),
                    )
                };
                let items = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, location)| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("navigation:{index}")),
                                label: location.path.display().to_string(),
                                detail: location.byte.to_string(),
                                source: source.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::PersistedLocation(location),
                        )
                    })
                    .collect();
                self.install_picker(title, items);
            }
            PickerKind::Commands => {
                let items = self
                    .commands
                    .specs()
                    .enumerate()
                    .map(|(index, spec)| {
                        let command = spec.id.as_str().to_owned();
                        let id = PickerItemId(format!("command:{index}:{command}"));
                        (
                            PickerItem {
                                id,
                                label: command.clone(),
                                detail: self
                                    .configuration
                                    .settings
                                    .command_labels
                                    .get(&command)
                                    .cloned()
                                    .unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(command),
                        )
                    })
                    .collect();
                self.install_picker(self.message("picker_commands_title", &[]), items);
            }
        }
    }

    fn sync_picker_ui(&mut self) {
        self.ui.picker = self.picker.as_ref().map(|picker| {
            let presentation = picker.presentation();
            PickerUiState {
                title: presentation.title,
                query: presentation.query,
                rows: presentation
                    .rows
                    .into_iter()
                    .map(|(id, label, detail, source)| PickerRow {
                        id: id.0,
                        label,
                        detail,
                        source,
                    })
                    .collect(),
                selected: presentation.selected,
                scroll: presentation.scroll,
                loading: presentation.loading,
            }
        });
    }

    fn picker_insert(&mut self, text: &str) {
        if let Some(picker) = &mut self.picker {
            picker.insert_query(text);
            self.sync_picker_ui();
        }
    }

    fn picker_backspace(&mut self) {
        if let Some(picker) = &mut self.picker {
            picker.backspace_query();
            self.sync_picker_ui();
        }
    }

    fn picker_move(&mut self, direction: i8) {
        if let Some(picker) = &mut self.picker {
            picker.move_selection(direction, self.configuration.settings.workspace.picker_wrap);
            let rows = usize::from(
                self.terminal_height
                    .saturating_mul(u16::from(
                        self.configuration.settings.ui.picker.height_percent,
                    ))
                    .saturating_div(100)
                    .saturating_sub(4),
            );
            picker.ensure_selected_visible(rows);
            self.sync_picker_ui();
        }
    }

    fn close_picker(&mut self) {
        self.picker = None;
        self.picker_payloads.clear();
        self.ui.picker = None;
    }

    fn accept_picker(&mut self) {
        let selected = self
            .picker
            .as_ref()
            .and_then(PickerState::selected_item)
            .map(|item| item.id.clone());
        let payload = selected
            .as_ref()
            .and_then(|id| self.picker_payloads.remove(id));
        self.close_picker();
        match payload {
            Some(PickerPayload::WorkspaceSearch(found)) => {
                self.navigate_workspace_search_match(found)
            }
            Some(PickerPayload::LspLocation(location)) => self.navigate_lsp_location(&location),
            Some(PickerPayload::Diagnostic { uri, range }) => {
                self.navigate_byte_location(&uri, range)
            }
            Some(PickerPayload::CodeAction(action)) => {
                if let Some(edit) = action.edit {
                    self.begin_workspace_proposal(
                        action.title,
                        edit,
                        None,
                        self.lsp_position_encoding(),
                    );
                }
            }
            Some(PickerPayload::Buffer(buffer)) => {
                if let Err(error) = self.state.show_buffer(buffer) {
                    self.show_error(error);
                }
            }
            Some(PickerPayload::WorkspaceRoot(root)) => self.activate_workspace_root(root),
            Some(PickerPayload::LspSession(key)) => self.select_lsp_session(&key),
            Some(PickerPayload::DapFrame(frame)) => {
                let _ = self.dap.control(DapControl::Scopes { frame_id: frame.id });
                if let Some(reference) = frame.source_reference {
                    let _ = self.dap.control(DapControl::Source { reference });
                } else if let Some(path) = frame.path {
                    self.navigate_matcher_source(MatcherSource {
                        path,
                        line: frame.line.saturating_sub(1) as usize,
                        column: frame.column.saturating_sub(1) as usize,
                    });
                }
            }
            Some(PickerPayload::MatcherSource(source)) => self.navigate_matcher_source(source),
            Some(PickerPayload::PersistedLocation(location)) => {
                self.navigate_persisted_location(location)
            }
            Some(PickerPayload::RegistrySummary(summary)) => self.request_registry(
                RegistryRequest::Show {
                    publisher: summary.publisher,
                    name: summary.name,
                    version: summary.version,
                },
                false,
            ),
            Some(PickerPayload::RegistryMetadata(metadata)) => {
                self.request_registry(RegistryRequest::Fetch { metadata }, true)
            }
            Some(PickerPayload::DapControl(control)) => {
                if let Err(error) = self.dap.control(control) {
                    self.show_error(error)
                }
            }
            Some(PickerPayload::ProposalFile(index)) => {
                if let Some(proposal) = &mut self.workspace_proposal {
                    let _ = proposal.toggle_file(index);
                    self.show_workspace_proposal();
                }
            }
            Some(PickerPayload::Command(command)) if !command.is_empty() => {
                if let Ok(invocation) = CommandInvocation::new(&command, InvocationSource::Picker) {
                    self.invoke(invocation);
                }
            }
            _ => {}
        }
    }

    fn navigate_workspace_search_match(&mut self, found: WorkspaceSearchMatch) {
        if self
            .state
            .buffers
            .iter()
            .any(|(_, buffer)| buffer.path.as_ref() == Some(&found.canonical_path))
        {
            if let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            {
                self.navigate_byte_location(&file_uri(&found.canonical_path), range);
            }
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.read_scoped(found.canonical_path, roots) {
            Ok(id) => {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::WorkspaceSearchNavigation { range: found.range },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    fn navigate_byte_location(&mut self, uri: &str, range: ByteRange) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer, _)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        if let Ok(view) = self.state.show_buffer(buffer) {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_precondition(Precondition::ViewDisplays { view, buffer })
                .with_operation(EditorOperation::SetCursor {
                    view,
                    position: range.start,
                })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
    }

    fn navigate_lsp_location(&mut self, location: &crate::languages::LspLocation) {
        let Some(path) = path_from_file_uri(&location.uri) else {
            self.show_error(self.message("lsp_invalid_uri", &[("uri", location.uri.clone())]));
            return;
        };
        if let Err(error) = self.workspace.require_path(&path) {
            self.show_error(error);
            return;
        }
        let encoding = self
            .lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            match self.vfs.read_scoped(path, roots) {
                Ok(id) => {
                    self.pending_vfs.insert(
                        id,
                        PendingVfs::LspNavigation {
                            range: location.range.clone(),
                            encoding,
                        },
                    );
                    self.ui.notification = Some(self.message("opening_file", &[]));
                    self.ui.notification_error = false;
                }
                Err(error) => self.show_error(error),
            }
            return;
        };
        let Ok(position) = PositionCodec::new(&buffer.text).lsp_to_byte(
            location.range.start.line,
            location.range.start.character,
            encoding,
        ) else {
            self.show_error(self.message("lsp_invalid_position", &[]));
            return;
        };
        if let Ok(view) = self.state.show_buffer(buffer_id) {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_precondition(Precondition::ViewDisplays {
                    view,
                    buffer: buffer_id,
                })
                .with_operation(EditorOperation::SetCursor { view, position })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
    }

    fn current_persisted_location(&self) -> Option<PersistedLocation> {
        let view = self.state.views.get(self.state.active_view?)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        Some(PersistedLocation {
            path: buffer.path.clone()?,
            byte: view.cursor.get(),
            anchor: view
                .selection
                .map_or(view.cursor.get(), |selection| selection.anchor.get()),
            scroll_line: view.scroll_line,
        })
    }
    fn push_change_location(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            if self.change_list.last().is_none_or(|previous| {
                previous.path != location.path || previous.byte != location.byte
            }) {
                self.change_list.push(location);
                let maximum = self.configuration.settings.persistence.changes;
                if self.change_list.len() > maximum {
                    self.change_list.drain(..self.change_list.len() - maximum);
                }
            }
        }
    }

    fn navigate_matcher_source(&mut self, source: MatcherSource) {
        if !source.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| source.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.read_scoped(source.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::MatcherNavigation { source });
            }
            Err(error) => self.show_error(error),
        }
    }

    fn navigate_persisted_location(&mut self, location: PersistedLocation) {
        if !location.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| location.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        if let Some(current) = self.current_persisted_location() {
            self.jump_list.push(current);
            let maximum = self.configuration.settings.persistence.jumps;
            if self.jump_list.len() > maximum {
                self.jump_list.drain(..self.jump_list.len() - maximum);
            }
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.read_scoped(location.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::PersistedNavigation { location });
            }
            Err(error) => self.show_error(error),
        }
    }

    fn navigate_diagnostic(&mut self, direction: i8) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(path) = self.state.buffers[buffer_id].path.as_ref() else {
            return;
        };
        let uri = crate::languages::file_uri(path);
        let target = self.diagnostics.adjacent(&uri, cursor, direction);
        let Some(target) = target else {
            self.show_error(self.message("diagnostic_none", &[]));
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: target.range.start,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ui.notification =
                Some(self.message("diagnostic_current", &[("message", target.message)]));
            self.ui.notification_error = false;
            self.ensure_cursor_visible();
        }
    }

    fn move_lsp_completion(&mut self, direction: i8) {
        let Some(completion) = &mut self.ui.language_completion else {
            return;
        };
        if completion.items.is_empty() {
            return;
        }
        completion.selected = if direction < 0 {
            completion
                .selected
                .checked_sub(1)
                .unwrap_or(completion.items.len() - 1)
        } else {
            (completion.selected + 1) % completion.items.len()
        };
    }

    fn accept_lsp_completion(&mut self) {
        let Some(completion) = &self.ui.language_completion else {
            return;
        };
        let Some(item) = self.lsp_completion_items.get(completion.selected).cloned() else {
            return;
        };
        let Some((buffer_id, document)) = self.active_lsp_document() else {
            return;
        };
        if !item.resolved
            && self
                .lsp_capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && let Some(resolve_item) = item.resolve_item.clone()
        {
            self.lsp_request_generation = self.lsp_request_generation.saturating_add(1);
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            let Some(server) = self.active_lsp_server.clone() else {
                return;
            };
            if let Err(error) = self.lsp.request(
                &server,
                LspFeature::CompletionResolve { item: resolve_item },
                document,
                offset,
                self.lsp_request_generation,
            ) {
                self.show_error(error)
            }
            return;
        }
        let encoding = self
            .lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let snippet = item.snippet;
        let session_allowed = item.additional_edits.is_empty();
        let edit = item.text_edit.unwrap_or_else(|| {
            let offset = self
                .state
                .active_view
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            let buffer = TextBuffer::from_text(&document.text);
            let (line, character) = PositionCodec::new(&buffer)
                .byte_to_lsp(offset, encoding)
                .unwrap_or((0, 0));
            crate::languages::LspTextEdit {
                range: crate::languages::LspRange {
                    start: crate::languages::LspPosition { line, character },
                    end: crate::languages::LspPosition { line, character },
                },
                new_text: item.insert_text,
            }
        });
        let mut edit = edit;
        let mut tabstops = Vec::new();
        if snippet {
            match crate::languages::parse_snippet(&edit.new_text, 1024 * 1024) {
                Ok(parsed) => {
                    edit.new_text = parsed.text;
                    tabstops = parsed.tabstops;
                }
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        }
        let insertion_start = TextBuffer::from_text(&document.text);
        let insertion_start = PositionCodec::new(&insertion_start)
            .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
            .ok();
        let mut edits = vec![edit];
        edits.extend(item.additional_edits);
        let workspace_edit = LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![crate::languages::LspDocumentEdit {
                uri: document.uri(),
                version: Some(document.version),
                edits,
            }],
        };
        if let Err(error) = self.apply_lsp_workspace_edit(workspace_edit) {
            self.show_error(error);
        } else if session_allowed
            && let Some(start) = insertion_start
            && !tabstops.is_empty()
        {
            let mut grouped = BTreeMap::<u32, Vec<ByteRange>>::new();
            for stop in tabstops {
                let start = ByteOffset(start.get().saturating_add(stop.start));
                let end = ByteOffset(insertion_start.unwrap().get().saturating_add(stop.end));
                if let Ok(range) = ByteRange::new(start, end) {
                    grouped.entry(stop.index).or_default().push(range);
                }
            }
            let mut groups = grouped.into_iter().collect::<Vec<_>>();
            groups.sort_by_key(|(index, _)| (*index == 0, *index));
            let groups = groups
                .into_iter()
                .map(|(_, ranges)| ranges)
                .collect::<Vec<_>>();
            if !groups.is_empty() {
                self.snippet_session = Some(SnippetSession {
                    buffer: buffer_id,
                    revision: self.state.buffers[buffer_id].text.revision(),
                    groups,
                    current: 0,
                });
                self.select_snippet_current();
            }
        }
        self.lsp_completion_items.clear();
        self.ui.language_completion = None;
    }

    fn prepare_snippet_mirror_transaction(
        &mut self,
        transaction: &mut EditorTransaction,
    ) -> Option<Vec<Vec<ByteRange>>> {
        let Some(session) = self.snippet_session.clone() else {
            return None;
        };
        let Some(buffer) = self.state.buffers.get(session.buffer) else {
            self.snippet_session = None;
            return None;
        };
        if buffer.text.revision() != session.revision {
            self.snippet_session = None;
            return None;
        }
        let Some(primary) = session
            .groups
            .get(session.current)
            .and_then(|group| group.first())
            .copied()
        else {
            self.snippet_session = None;
            return None;
        };
        let mut target = None;
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation {
                if *buffer != session.buffer || target.is_some() {
                    self.snippet_session = None;
                    return None;
                }
                target = Some(edits.clone());
            }
        }
        let Some(edits) = target else { return None };
        if edits.base_revision() != session.revision
            || edits.edits().is_empty()
            || edits
                .edits()
                .iter()
                .any(|edit| edit.range().start < primary.start || edit.range().end > primary.end)
        {
            self.snippet_session = None;
            return None;
        }
        let Ok(mut replacement) = buffer.text.slice(primary) else {
            self.snippet_session = None;
            return None;
        };
        let mut local = edits
            .edits()
            .iter()
            .map(|edit| {
                (
                    edit.range().start.get() - primary.start.get(),
                    edit.range().end.get() - primary.start.get(),
                    edit.replacement().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        for (start, end, text) in local {
            if !replacement.is_char_boundary(start) || !replacement.is_char_boundary(end) {
                self.snippet_session = None;
                return None;
            }
            replacement.replace_range(start..end, &text);
        }
        let mut mirrors = session.groups[session.current].clone();
        mirrors.sort_by_key(|range| range.start);
        let replacement_edits = mirrors
            .iter()
            .map(|range| TextEdit::new(*range, replacement.clone()))
            .collect::<Vec<_>>();
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation
                && *buffer == session.buffer
            {
                *edits = TextEditSet::new(session.revision, replacement_edits.clone());
            }
        }
        let shift = |position: ByteOffset| {
            let mut value = position.get() as i128;
            for range in &mirrors {
                if range.end <= position {
                    value += replacement.len() as i128 - range.len() as i128;
                }
            }
            ByteOffset(value.max(0) as usize)
        };
        let mut groups = Vec::with_capacity(session.groups.len());
        for group in &session.groups {
            let mut updated = Vec::with_capacity(group.len());
            for range in group {
                if mirrors.contains(range) {
                    let start = shift(range.start);
                    updated.push(
                        ByteRange::new(start, ByteOffset(start.get() + replacement.len())).ok()?,
                    );
                } else {
                    updated.push(ByteRange::new(shift(range.start), shift(range.end)).ok()?);
                }
            }
            groups.push(updated);
        }
        Some(groups)
    }

    fn select_snippet_current(&mut self) {
        let Some(session) = self.snippet_session.as_ref() else {
            return;
        };
        if self
            .state
            .buffers
            .get(session.buffer)
            .is_none_or(|buffer| buffer.text.revision() != session.revision)
        {
            self.snippet_session = None;
            return;
        }
        let Some(range) = session
            .groups
            .get(session.current)
            .and_then(|group| group.first())
            .copied()
        else {
            return;
        };
        let Some(view) = self
            .state
            .active_view
            .filter(|view| self.state.views[*view].buffer == session.buffer)
        else {
            return;
        };
        self.state.views[view].cursor = range.end;
        self.state.views[view].selection = Some(crate::editing::Selection {
            anchor: range.start,
            head: range.end,
            kind: crate::editing::SelectionKind::Character,
        });
        self.ensure_cursor_visible();
    }
    fn navigate_snippet(&mut self, direction: i8) {
        let Some(session) = self.snippet_session.as_mut() else {
            return;
        };
        if direction > 0 {
            if session.current + 1 >= session.groups.len() {
                self.snippet_session = None;
                return;
            }
            session.current += 1
        } else if direction < 0 {
            session.current = session.current.saturating_sub(1)
        }
        self.select_snippet_current();
    }

    fn begin_workspace_proposal(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<(String, crate::languages::ServerRequestId)>,
        encoding: PositionEncoding,
    ) {
        let mut create_paths = Vec::new();
        let mut resource_paths = Vec::<(PathBuf, Option<PathBuf>)>::new();
        for resource in &edit.resources {
            match resource {
                crate::languages::LspResourceOperation::Create {
                    uri,
                    overwrite,
                    ignore_if_exists,
                } if !overwrite && !ignore_if_exists => {
                    let Some(path) = path_from_file_uri(uri) else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    create_paths.push(path)
                }
                crate::languages::LspResourceOperation::Rename {
                    old_uri,
                    new_uri,
                    overwrite,
                    ignore_if_exists,
                } if !overwrite && !ignore_if_exists => {
                    let (Some(source), Some(target)) =
                        (path_from_file_uri(old_uri), path_from_file_uri(new_uri))
                    else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    resource_paths.push((source, Some(target)));
                }
                crate::languages::LspResourceOperation::Delete {
                    uri,
                    recursive,
                    ignore_if_not_exists,
                } if !recursive && !ignore_if_not_exists => {
                    let Some(source) = path_from_file_uri(uri) else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    resource_paths.push((source, None));
                }
                _ => {
                    if let Some((server, request_id)) = &response {
                        let _ = self.lsp.respond_apply_edit(
                            server,
                            request_id.clone(),
                            false,
                            Some(self.message("workspace_resource_unsupported", &[])),
                        );
                    }
                    self.show_error(self.message("workspace_resource_unsupported", &[]));
                    return;
                }
            }
        }
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            if let Some((server, request_id)) = response {
                let _ = self.lsp.respond_apply_edit(
                    &server,
                    request_id,
                    false,
                    Some("another workspace edit is already being previewed".into()),
                );
            }
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.saturating_add(1);
        self.workspace_proposal_root_revision = Some(self.workspace.root_revision());
        let mut pending = PendingWorkspaceProposal {
            id,
            encoding,
            label,
            documents: edit.documents,
            snapshots: Vec::new(),
            creates: Vec::new(),
            resource_intents: Vec::new(),
            resolved_paths: HashMap::new(),
            waiting: 0,
            response,
        };
        for document in pending.documents.clone() {
            let Some(path) = path_from_file_uri(&document.uri) else {
                self.reject_pending_workspace_proposal(
                    pending,
                    format!("unsupported workspace-edit URI `{}`", document.uri),
                );
                return;
            };
            if self.workspace.require_path(&path).is_err() {
                self.reject_pending_workspace_proposal(
                    pending,
                    format!(
                        "workspace edit target is outside workspace roots: {}",
                        path.display()
                    ),
                );
                return;
            }
            if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                pending.resolved_paths.insert(path.clone(), path.clone());
                pending.snapshots.push(FileSnapshot {
                    canonical_path: path,
                    revision: Some(buffer.text.revision()),
                    bytes: buffer.text.to_bytes(),
                });
            } else {
                let roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect();
                match self.vfs.read_scoped(path.clone(), roots) {
                    Ok(request) => {
                        self.pending_vfs.insert(
                            request,
                            PendingVfs::WorkspaceProposalRead {
                                proposal: id,
                                requested_path: path,
                            },
                        );
                        pending.waiting = pending.waiting.saturating_add(1);
                    }
                    Err(error) => {
                        self.reject_pending_workspace_proposal(pending, error.to_string());
                        return;
                    }
                }
            }
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        for path in create_paths {
            match self.vfs.resolve_create(path.clone(), roots.clone()) {
                Ok(request) => {
                    self.pending_vfs.insert(
                        request,
                        PendingVfs::WorkspaceProposalCreate {
                            proposal: id,
                            requested_path: path,
                        },
                    );
                    pending.waiting = pending.waiting.saturating_add(1)
                }
                Err(error) => {
                    self.reject_pending_workspace_proposal(pending, error.to_string());
                    return;
                }
            }
        }
        for (source, target) in resource_paths {
            match self.vfs.resolve_resource(
                source,
                target,
                roots.clone(),
                self.configuration
                    .settings
                    .workspace
                    .search
                    .maximum_file_bytes as usize,
            ) {
                Ok(request) => {
                    self.pending_vfs.insert(
                        request,
                        PendingVfs::WorkspaceResourceResolve { proposal: id },
                    );
                    pending.waiting = pending.waiting.saturating_add(1)
                }
                Err(error) => {
                    self.reject_pending_workspace_proposal(pending, error.to_string());
                    return;
                }
            }
        }
        self.pending_workspace_proposal = Some(pending);
        if self
            .pending_workspace_proposal
            .as_ref()
            .is_some_and(|pending| pending.waiting == 0)
        {
            self.finish_workspace_proposal();
        } else {
            self.ui.notification = Some(self.message("workspace_edit_loading", &[]));
            self.ui.notification_error = false;
        }
    }

    fn reject_pending_workspace_proposal(
        &mut self,
        pending: PendingWorkspaceProposal,
        reason: String,
    ) {
        if let Some((server, request_id)) = pending.response {
            let _ = self
                .lsp
                .respond_apply_edit(&server, request_id, false, Some(reason.clone()));
        }
        self.show_error(self.message("workspace_edit_failed", &[("diagnostic", reason)]));
    }

    fn finish_workspace_proposal(&mut self) {
        let Some(pending) = self.pending_workspace_proposal.take() else {
            return;
        };
        let encoding = pending.encoding;
        let mut drafts = pending
            .documents
            .iter()
            .filter_map(|document| {
                let requested = path_from_file_uri(&document.uri)?;
                let canonical = pending.resolved_paths.get(&requested)?.clone();
                let snapshot = pending
                    .snapshots
                    .iter()
                    .find(|snapshot| snapshot.canonical_path == canonical)?;
                let text = TextBuffer::from_bytes(&snapshot.bytes).ok()?;
                let codec = PositionCodec::new(&text);
                let edits = document
                    .edits
                    .iter()
                    .map(|edit| {
                        let start = codec
                            .lsp_to_byte(
                                edit.range.start.line,
                                edit.range.start.character,
                                encoding,
                            )
                            .map_err(|error| error.to_string())?;
                        let end = codec
                            .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                            .map_err(|error| error.to_string())?;
                        Ok(TextEdit::new(
                            ByteRange::new(start, end).map_err(|error| error.to_string())?,
                            edit.new_text.clone(),
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
                    .ok()?;
                Some(WorkspaceEditDraft {
                    canonical_path: canonical,
                    expected_revision: document.version.map(BufferRevision).or(snapshot.revision),
                    edits,
                })
            })
            .collect::<Vec<_>>();
        if drafts.len() != pending.documents.len() {
            self.reject_pending_workspace_proposal(
                pending,
                "workspace edit contains invalid positions or missing snapshots".into(),
            );
            return;
        }
        for path in &pending.creates {
            drafts.push(WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: None,
                edits: Vec::new(),
            });
        }
        match WorkspaceEditProposal::build(
            pending.id,
            pending.label.clone(),
            &self.workspace,
            drafts,
            pending.snapshots.clone(),
        ) {
            Ok(proposal) => {
                self.workspace_resource_intents = pending.resource_intents.clone();
                self.workspace_proposal_response = pending.response.clone();
                self.workspace_proposal = Some(proposal);
                self.show_workspace_proposal();
            }
            Err(error) => self.reject_pending_workspace_proposal(pending, error.to_string()),
        }
    }

    fn show_workspace_proposal(&mut self) {
        let Some(proposal) = &self.workspace_proposal else {
            return;
        };
        let mut items = proposal
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let id = PickerItemId(format!(
                    "proposal-file:{index}:{}",
                    file.canonical_path.display()
                ));
                (
                    PickerItem {
                        id,
                        label: file.canonical_path.display().to_string(),
                        detail: self.message(
                            if file.selected {
                                "workspace_edit_included"
                            } else {
                                "workspace_edit_excluded"
                            },
                            &[("count", file.edit_count.to_string())],
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::ProposalFile(index),
                )
            })
            .collect::<Vec<_>>();
        items.extend(
            self.workspace_resource_intents
                .iter()
                .enumerate()
                .map(|(index, intent)| {
                    let detail = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => self.message(
                            "workspace_resource_rename_row",
                            &[("target", target.display().to_string())],
                        ),
                        ResourceIntentKind::Delete => {
                            self.message("workspace_resource_delete_row", &[])
                        }
                    };
                    (
                        PickerItem {
                            id: PickerItemId(format!("resource-intent:{index}")),
                            label: intent.source.display().to_string(),
                            detail,
                            source: self.message("workspace_resource_staged", &[]),
                            action: PickerAction::None,
                        },
                        PickerPayload::Command(String::new()),
                    )
                }),
        );
        let title = proposal.label.clone();
        self.install_picker(title, items);
    }

    fn reject_workspace_proposal(&mut self) {
        if let Some(mut proposal) = self.workspace_proposal.take() {
            let _ = proposal.reject();
        }
        if let Some((server, request_id)) = self.workspace_proposal_response.take() {
            let _ = self.lsp.respond_apply_edit(
                &server,
                request_id,
                false,
                Some("workspace edit rejected by user".into()),
            );
        }
        self.workspace_proposal_root_revision = None;
        self.workspace_resource_intents.clear();
        self.close_picker();
        self.ui.notification = Some(self.message("workspace_edit_rejected", &[]));
        self.ui.notification_error = false;
    }

    fn accept_workspace_proposal(&mut self) {
        if self.workspace_proposal_root_revision != Some(self.workspace.root_revision()) {
            self.reject_workspace_proposal();
            self.show_error(self.message("workspace_root_stale", &[]));
            return;
        }
        let Some(proposal) = &self.workspace_proposal else {
            return;
        };
        let selected = proposal
            .files
            .iter()
            .filter(|file| file.selected)
            .cloned()
            .collect::<Vec<_>>();
        let mut targets = Vec::new();
        let mut created_buffers = Vec::new();
        for file in &selected {
            let buffer = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&file.canonical_path))
                .map(|(buffer, _)| buffer)
                .unwrap_or_else(|| {
                    let text = TextBuffer::from_bytes(&file.original_bytes)
                        .expect("validated workspace proposal snapshot remains valid");
                    self.state
                        .load_hidden_text(text, file.canonical_path.clone())
                });
            if file.base_revision.is_none() {
                created_buffers.push(buffer);
            }
            let current = self.state.buffers[buffer].text.revision();
            if file
                .base_revision
                .is_some_and(|expected| expected != current)
            {
                self.show_error(self.message(
                    "workspace_edit_failed",
                    &[(
                        "diagnostic",
                        format!("{} changed after preview", file.canonical_path.display()),
                    )],
                ));
                return;
            }
            let replacement = match TextBuffer::from_bytes(&file.proposed_bytes) {
                Ok(text) => text.to_string(),
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            targets.push((buffer, current, replacement));
        }
        let mut transaction = EditorTransaction::new(self.transaction_id());
        transaction.label = self
            .workspace_proposal
            .as_ref()
            .map(|proposal| proposal.label.clone());
        for (buffer, revision, replacement) in targets {
            transaction
                .preconditions
                .push(Precondition::BufferRevision { buffer, revision });
            transaction
                .preconditions
                .push(Precondition::BufferWritable(buffer));
            let end = ByteOffset(self.state.buffers[buffer].text.len_bytes());
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    revision,
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), end).expect("ordered range"),
                        replacement,
                    )],
                ),
            });
        }
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        for buffer in created_buffers {
            if let Some(state) = self.state.buffers.get_mut(buffer) {
                state.dirty = true;
            }
        }
        if let Some(proposal) = &mut self.workspace_proposal {
            let _ = proposal.accept();
        }
        self.workspace_proposal = None;
        self.workspace_proposal_root_revision = None;
        self.accepted_resource_intents
            .extend(std::mem::take(&mut self.workspace_resource_intents));
        if let Some((server, request_id)) = self.workspace_proposal_response.take() {
            let _ = self.lsp.respond_apply_edit(&server, request_id, true, None);
        }
        self.close_picker();
        self.ui.notification = Some(self.message("workspace_edit_applied", &[]));
        self.ui.notification_error = false;
    }

    fn apply_lsp_workspace_edit(&mut self, edit: LspWorkspaceEdit) -> Result<(), String> {
        let encoding = self
            .lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let mut operations = Vec::new();
        let mut preconditions = Vec::new();
        for document in edit.documents {
            let path = path_from_file_uri(&document.uri)
                .ok_or_else(|| format!("unsupported workspace-edit URI `{}`", document.uri))?;
            let (buffer_id, buffer) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                .ok_or_else(|| {
                    format!(
                        "workspace edit targets unopened file `{}`; preview required",
                        path.display()
                    )
                })?;
            let revision = buffer.text.revision();
            if document
                .version
                .is_some_and(|version| version != revision.0)
            {
                return Err(format!("workspace edit for `{}` is stale", path.display()));
            }
            let codec = PositionCodec::new(&buffer.text);
            let mut edits = Vec::with_capacity(document.edits.len());
            for edit in document.edits {
                let start = codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .map_err(|error| error.to_string())?;
                let end = codec
                    .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                    .map_err(|error| error.to_string())?;
                let range = ByteRange::new(start, end).map_err(|error| error.to_string())?;
                edits.push(TextEdit::new(range, edit.new_text));
            }
            buffer
                .text
                .validate_edits(&TextEditSet::new(revision, edits.clone()))
                .map_err(|error| error.to_string())?;
            preconditions.push(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            });
            preconditions.push(Precondition::BufferWritable(buffer_id));
            operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, edits),
            });
        }
        let mut transaction = EditorTransaction::new(self.transaction_id());
        transaction.preconditions = preconditions;
        transaction.operations = operations;
        transaction.label = Some("LSP workspace edit".into());
        self.state
            .apply_transaction(&transaction)
            .map_err(|error| error.to_string())?;
        self.ui.notification = Some(self.message("lsp_edit_applied", &[]));
        self.ui.notification_error = false;
        Ok(())
    }

    fn start_dap_configuration(&mut self, name: &str) {
        let Some(session) = self
            .configuration
            .settings
            .debug
            .sessions
            .get(name)
            .cloned()
        else {
            self.show_error(format!("unknown debug configuration `{name}`"));
            return;
        };
        let Some(adapter) = self
            .configuration
            .settings
            .debug
            .adapters
            .get(&session.adapter)
            .cloned()
        else {
            self.show_error("debug adapter configuration is missing");
            return;
        };
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::StartDapConfiguration(name.into()));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::DebugProgram)
        {
            self.show_error(error);
            return;
        }
        let Some(root_id) = self.workspace.active_root() else {
            return;
        };
        let Some(root) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let cwd = session.cwd.clone().map_or_else(
            || root.clone(),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            },
        );
        let request = ExecutionPreflightRequest {
            executable: adapter.program.clone(),
            cwd,
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::DapAdapter {
                        name: name.into(),
                        adapter,
                        session,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    fn launch_preflighted_dap(
        &mut self,
        name: String,
        adapter: crate::configuration::DebugAdapterConfig,
        session: crate::configuration::DebugSessionConfig,
        adapter_executable: PathBuf,
        cwd: PathBuf,
        root_revision: u64,
        program: Option<PathBuf>,
    ) {
        if root_revision != self.workspace.root_revision() {
            return;
        }
        let request = match session.request {
            crate::configuration::DebugRequestKind::Launch => "launch",
            crate::configuration::DebugRequestKind::Attach => "attach",
        };
        let path_maps = session.path_maps.clone();
        let terminal_profile = session.run_in_terminal_profile.clone();
        let mut arguments = serde_json::Map::from_iter(session.arguments);
        arguments.insert(
            "cwd".into(),
            serde_json::Value::String(cwd.display().to_string()),
        );
        if let Some(program) = program {
            arguments.insert(
                "program".into(),
                serde_json::Value::String(program.display().to_string()),
            );
        }
        let environment = adapter.environment.into_iter().collect();
        match self.dap.start_absolute(
            session.adapter,
            adapter_executable,
            adapter.arguments,
            environment,
            path_maps,
            cwd,
            request.into(),
            serde_json::Value::Object(arguments),
        ) {
            Ok(()) => {
                self.active_debug_configuration = Some(name.clone());
                self.active_debug_terminal_profile = terminal_profile;
                self.ui.notification = Some(self.message("dap_started", &[("adapter", name)]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn start_dap(&mut self, requested: Option<String>, program: Option<PathBuf>) {
        let Some((_, document)) = self.active_lsp_document() else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        let entry = requested
            .as_ref()
            .and_then(|id| self.tools.iter().find(|entry| entry.definition.id == *id))
            .or_else(|| {
                self.tools.iter().find(|entry| {
                    entry.definition.kind == ToolKind::Dap
                        && entry.definition.languages.contains(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
            });
        let Some(entry) = entry else {
            self.show_error(self.message("dap_no_adapter", &[]));
            return;
        };
        let ToolStatus::Installed(adapter_executable) = &entry.status else {
            self.show_error(self.message("dap_no_adapter", &[]));
            return;
        };
        let definition = entry.definition.clone();
        let adapter_executable = adapter_executable.clone();
        if !self.execution_revalidated {
            self.defer_trusted_execution(DeferredExecution::StartDap {
                requested: Some(definition.id.clone()),
                program,
            });
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::DebugProgram)
        {
            self.show_error(error);
            return;
        }
        let program = program.unwrap_or(document.path);
        let Ok(root_id) = self.workspace.require_path(&program) else {
            self.show_error(self.message("dap_no_buffer", &[]));
            return;
        };
        let Some(cwd) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        let request = ExecutionPreflightRequest {
            executable: adapter_executable,
            cwd,
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.workspace.root_revision(),
        };
        match self.execution_preflight.request(request) {
            Ok(id) => {
                self.pending_execution_preflight.insert(
                    id,
                    PendingExecutionPreflight::DapToolAdapter {
                        definition,
                        program,
                    },
                );
            }
            Err(error) => self.show_error(error),
        }
    }

    fn process_dap_events(&mut self) {
        while let Some(event) = self.dap.try_event() {
            match event {
                DapEvent::Starting { .. } => {}
                DapEvent::Running { adapter } => {
                    self.ui.notification =
                        Some(self.message("dap_running", &[("adapter", adapter)]));
                    self.ui.notification_error = false;
                }
                DapEvent::Stopped {
                    adapter, reason, ..
                } => {
                    self.ui.notification = Some(
                        self.message("dap_stopped", &[("adapter", adapter), ("reason", reason)]),
                    );
                    self.ui.notification_error = false;
                    for expression in self.dap_watches.iter().take(100) {
                        let _ = self.dap.control(DapControl::Evaluate {
                            expression: expression.clone(),
                            frame_id: None,
                            context: "watch".into(),
                        });
                    }
                }
                DapEvent::Output { adapter, output } => {
                    self.dap_console
                        .push_back((adapter.clone(), output.clone()));
                    while self.dap_console.len() > 10_000 {
                        self.dap_console.pop_front();
                    }
                    self.ui.notification = Some(self.message(
                        "dap_output",
                        &[("adapter", adapter), ("output", output.replace('\n', " "))],
                    ));
                    self.ui.notification_error = false;
                }
                DapEvent::Response {
                    stop_generation,
                    response,
                    ..
                } => {
                    self.dap_responses
                        .push_back((stop_generation, response.clone()));
                    while self.dap_responses.len() > 256 {
                        self.dap_responses.pop_front();
                    }
                    match response {
                        DapResponse::Threads(rows) => self.open_dap_picker(
                            "threads",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        row.name,
                                        row.id.to_string(),
                                        DapControl::StackTrace {
                                            thread_id: row.id,
                                            start: 0,
                                            levels: 256,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Stack(rows) => self.open_dap_frames_picker(rows),
                        DapResponse::Scopes(rows) => self.open_dap_picker(
                            "scopes",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        row.name,
                                        if row.expensive {
                                            "expensive".into()
                                        } else {
                                            String::new()
                                        },
                                        DapControl::Variables {
                                            reference: row.variables_reference,
                                            start: 0,
                                            count: 1000,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Variables(rows) => self.open_dap_picker(
                            "variables",
                            rows.into_iter()
                                .map(|row| {
                                    (
                                        format!("{} = {}", row.name, row.value),
                                        row.type_name.unwrap_or_default(),
                                        DapControl::Variables {
                                            reference: row.variables_reference,
                                            start: 0,
                                            count: 1000,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        DapResponse::Evaluate(value) => {
                            self.ui.notification = Some(self.message(
                                "dap_evaluate_result",
                                &[("name", value.name), ("value", value.value)],
                            ));
                            self.ui.notification_error = false;
                        }
                        DapResponse::Breakpoints(rows) => {
                            self.ui.notification = Some(self.message(
                                "dap_breakpoints_synced",
                                &[("count", rows.len().to_string())],
                            ));
                            self.ui.notification_error = false;
                        }
                        DapResponse::Source {
                            name,
                            content,
                            mime_type,
                        } => {
                            let (buffer, _) =
                                self.state.open_text(TextBuffer::from_text(&content), None);
                            self.state.buffers[buffer].read_only = true;
                            self.state.buffers[buffer].dirty = false;
                            self.ui.notification = Some(self.message(
                                "dap_source_loaded",
                                &[("name", name), ("mime", mime_type.unwrap_or_default())],
                            ));
                            self.ui.notification_error = false;
                        }
                    }
                }
                DapEvent::Terminated { adapter } => {
                    let _ = self.dap.stop();
                    self.ui.notification = Some(self.message(
                        "dap_stopped",
                        &[("adapter", adapter), ("reason", "terminated".into())],
                    ));
                    self.ui.notification_error = false;
                }
                DapEvent::RunInTerminal {
                    request_seq,
                    cwd,
                    arguments,
                    environment,
                    ..
                } => {
                    let Some(profile) = self.active_debug_terminal_profile.clone() else {
                        let _ = self.dap.control(DapControl::ReverseResponse {
                            request_seq,
                            success: false,
                            message: Some(
                                "runInTerminal is not enabled by the debug configuration".into(),
                            ),
                            process_id: None,
                        });
                        continue;
                    };
                    let Some(config) = self
                        .configuration
                        .settings
                        .terminals
                        .profiles
                        .get(&profile)
                        .cloned()
                    else {
                        continue;
                    };
                    let root = self
                        .workspace
                        .active_root()
                        .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                        .map(|root| root.identity.canonical_path.clone());
                    let expected_cwd = config.cwd.clone().map_or_else(
                        || root.clone().unwrap_or_default(),
                        |path| {
                            if path.is_absolute() {
                                path
                            } else {
                                root.clone().unwrap_or_default().join(path)
                            }
                        },
                    );
                    let valid = arguments.first()
                        == config.program.to_str().map(str::to_owned).as_ref()
                        && arguments.get(1..) == Some(config.arguments.as_slice())
                        && cwd == expected_cwd
                        && environment == config.environment.clone().into_iter().collect();
                    if !valid {
                        let _=self.dap.control(DapControl::ReverseResponse{request_seq,success:false,message:Some("runInTerminal request differs from the configured terminal profile".into()),process_id:None});
                        continue;
                    }
                    self.execution_revalidated = false;
                    self.defer_trusted_execution(DeferredExecution::OpenDapTerminal {
                        profile,
                        request_seq,
                    });
                }
                DapEvent::Failed { adapter, message } => {
                    let _ = self.dap.stop();
                    self.show_error(self.message(
                        "dap_failed",
                        &[("adapter", adapter), ("diagnostic", message)],
                    ));
                }
            }
        }
    }

    fn recovery_path(&self, buffer: crate::editing::BufferId) -> PathBuf {
        let identifier = format!("{buffer:?}")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        self.recovery_directory
            .join(format!("buffer-{identifier}.toml"))
    }

    fn schedule_recovery_work(&mut self) {
        if !self.configuration.settings.recovery.enabled {
            return;
        }
        if !self.recovery_discovery_queued
            && let Ok(id) = self.vfs.discover_recovery(
                self.recovery_directory.clone(),
                self.configuration
                    .settings
                    .recovery
                    .maximum_discovered_records,
            )
        {
            self.recovery_discovery_queued = true;
            self.pending_vfs.insert(id, PendingVfs::RecoveryDiscovery);
        }
        if self.configuration.settings.recovery.cleanup_enabled
            && !self.recovery_cleanup_queued
            && let Ok(id) = self.vfs.cleanup_recovery(
                self.recovery_directory.clone(),
                self.configuration
                    .settings
                    .recovery
                    .maximum_retained_records,
                self.configuration.settings.recovery.maximum_age_days,
            )
        {
            self.recovery_cleanup_queued = true;
            self.pending_vfs.insert(id, PendingVfs::RecoveryCleanup);
        }
        let now = Instant::now();
        let debounce =
            Duration::from_millis(self.configuration.settings.recovery.write_debounce_ms);
        let buffers = self
            .state
            .buffers
            .iter()
            .map(|(id, buffer)| (id, buffer.dirty, buffer.text.revision()))
            .collect::<Vec<_>>();
        for (buffer, dirty, revision) in buffers {
            if dirty {
                let already_queued = self.recovery_revisions.get(&buffer) == Some(&revision)
                    || self.recovery_pruned_revisions.get(&buffer) == Some(&revision);
                let already_scheduled = self
                    .recovery_scheduled
                    .get(&buffer)
                    .is_some_and(|(scheduled, _)| *scheduled == revision);
                if !already_queued && !already_scheduled {
                    self.recovery_scheduled
                        .insert(buffer, (revision, now + debounce));
                }
            } else {
                self.recovery_scheduled.remove(&buffer);
                self.recovery_pruned_revisions.remove(&buffer);
                if self.configuration.settings.recovery.delete_clean_journals
                    && self.recovery_revisions.contains_key(&buffer)
                {
                    let journal = self.recovery_path(buffer);
                    let delete_queued = self.pending_vfs.values().any(|pending| {
                        matches!(pending, PendingVfs::RecoveryDelete { path, .. } if path == &journal)
                    });
                    if !delete_queued && let Ok(id) = self.vfs.delete(journal.clone()) {
                        self.pending_vfs.insert(
                            id,
                            PendingVfs::RecoveryDelete {
                                path: journal,
                                buffer: Some((buffer, self.recovery_revisions[&buffer])),
                            },
                        );
                    }
                }
            }
        }
        let due = self
            .recovery_scheduled
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(buffer, (revision, _))| (*buffer, *revision))
            .collect::<Vec<_>>();
        for (buffer, revision) in due {
            let Some(buffer_state) = self.state.buffers.get(buffer) else {
                self.recovery_scheduled.remove(&buffer);
                continue;
            };
            if !buffer_state.dirty || buffer_state.text.revision() != revision {
                self.recovery_scheduled.remove(&buffer);
                continue;
            }
            let record = RecoveryRecord::new(
                buffer_state.path.as_deref(),
                revision.0,
                buffer_state.text.to_string(),
            );
            let journal = self.recovery_path(buffer);
            if let Ok(id) = self.vfs.write_recovery(journal, record) {
                self.recovery_scheduled.remove(&buffer);
                self.recovery_pruned_revisions.remove(&buffer);
                self.recovery_revisions.insert(buffer, revision);
                self.pending_vfs
                    .insert(id, PendingVfs::RecoveryWrite { buffer, revision });
            }
        }
    }

    fn queue_dirty_recovery_before_exit(&mut self) {
        if !self.configuration.settings.recovery.enabled {
            return;
        }
        let now = Instant::now();
        for (buffer, state) in &self.state.buffers {
            if state.dirty
                && self.recovery_revisions.get(&buffer) != Some(&state.text.revision())
                && self.recovery_pruned_revisions.get(&buffer) != Some(&state.text.revision())
            {
                self.recovery_scheduled
                    .insert(buffer, (state.text.revision(), now));
            }
        }
        self.schedule_recovery_work();
    }

    fn update_notification_timer(&mut self) {
        if self.ui.notification != self.observed_notification {
            self.observed_notification = self.ui.notification.clone();
            self.notification_deadline = self.ui.notification.as_ref().and_then(|_| {
                let timeout = self.configuration.settings.ui.notifications.timeout_ms;
                (timeout > 0).then(|| Instant::now() + Duration::from_millis(timeout))
            });
        }
        if self
            .notification_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.ui.notification = None;
            self.ui.notification_error = false;
            self.observed_notification = None;
            self.notification_deadline = None;
        }
    }

    fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Resize(width, height) => {
                if let Some(id) = self.active_terminal {
                    let rows = height.saturating_sub(2);
                    let _ = self.ptys.resize(id, width, rows);
                    if let Some(terminal) = self.terminals.get_mut(&id) {
                        terminal.screen.resize(width, rows);
                    }
                    self.sync_terminal_panel();
                }
                self.terminal_width = width;
                self.terminal_height = height;
                if self.configuration.settings.ui.keep_cursor_visible_on_resize {
                    self.ensure_cursor_visible();
                }
            }
            UiEvent::Key { key, kind } => match kind {
                KeyInputKind::Press | KeyInputKind::Repeat => self.handle_configured_key(key),
                KeyInputKind::Release => self.handle_key_in_context("key_release", key),
            },
            UiEvent::Paste(text) => self.handle_configured_paste(text),
            UiEvent::FocusGained => self.handle_key_in_context("focus", KeyEvent::focus_gained()),
            UiEvent::FocusLost => {
                if self
                    .ui
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.kind == PromptKind::WorkspaceTrust)
                {
                    self.close_prompt();
                }
                self.handle_key_in_context("focus", KeyEvent::focus_lost());
            }
            UiEvent::Mouse { key, column, row } => {
                self.mouse_position = Some((column, row));
                let over_explorer = self.ui.explorer.is_some()
                    && crate::terminal_ui::explorer_panel_width(
                        &self.configuration.settings,
                        self.terminal_width,
                    )
                    .is_some_and(|panel| column <= panel);
                let context = if self.ui.picker.is_some() {
                    "picker_mouse"
                } else if self.ui.prompt.is_some() {
                    "prompt_mouse"
                } else if self.ui.tool_menu.is_some() {
                    "tool_manager_mouse"
                } else if over_explorer {
                    self.ui.explorer_focused = true;
                    "explorer_mouse"
                } else if self.ui.dashboard || self.state.active_view.is_none() {
                    "dashboard_mouse"
                } else {
                    self.ui.explorer_focused = false;
                    "editor_mouse"
                };
                self.handle_key_in_context(context, key);
            }
        }
    }

    fn active_input_context(&self) -> String {
        if let Some(pending) = self.input.pending_context() {
            return match pending {
                crate::input::PendingInputContext::Register => "register",
                crate::input::PendingInputContext::Prefix => "prefix",
                crate::input::PendingInputContext::Replace => "replace",
                crate::input::PendingInputContext::MacroRecord => "macro_record",
                crate::input::PendingInputContext::MacroReplay => "macro_replay",
            }
            .into();
        }
        if self.active_terminal.is_some() {
            return "terminal".into();
        }
        if self.workspace_proposal.is_some() {
            return "workspace_edit".into();
        }
        if self.ui.explorer.is_some() && self.ui.explorer_focused {
            return "explorer".into();
        }
        if self.ui.picker.is_some() {
            return "picker".into();
        }
        if self.ui.language_completion.is_some() {
            return "completion".into();
        }
        if let Some(prompt) = &self.ui.prompt {
            return match prompt.kind {
                PromptKind::Command => "command_prompt",
                PromptKind::SearchForward | PromptKind::SearchBackward => "search_prompt",
                PromptKind::WorkspaceTrust => "workspace_trust_prompt",
            }
            .into();
        }
        if self.ui.tool_menu.is_some() {
            return "tool_manager".into();
        }
        if self.ui.dashboard || self.state.active_view.is_none() {
            return "dashboard".into();
        }
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        format!("editor.{}", mode_name(mode))
    }

    fn handle_configured_key(&mut self, key: KeyEvent) {
        let context = self.active_input_context();
        self.handle_key_in_context(&context, key);
    }

    fn handle_key_in_context(&mut self, context: &str, key: KeyEvent) {
        let context = context.to_owned();
        let resolution = self
            .configuration
            .keymaps
            .resolvers
            .get_mut(&context)
            .map_or(MappingResolution::Missing, |resolver| {
                resolver.feed(key.clone())
            });
        match resolution {
            MappingResolution::Ready(invocation) => {
                self.pending_keymap = None;
                self.invoke_mapped(invocation);
            }
            MappingResolution::ReadyAndReplay(invocation, replay) => {
                self.pending_keymap = None;
                self.invoke_mapped(invocation);
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Replay(replay) => {
                self.pending_keymap = None;
                for key in replay {
                    self.handle_configured_key(key);
                }
            }
            MappingResolution::Disabled => self.pending_keymap = None,
            MappingResolution::Pending | MappingResolution::Ambiguous => {
                self.pending_keymap = Some((
                    Instant::now()
                        + Duration::from_millis(
                            self.configuration.settings.input.mapping_timeout_ms,
                        ),
                    context,
                ));
                return;
            }
            MappingResolution::Missing => {
                self.pending_keymap = None;
                let mut handled = false;
                if let Some(character) = key.text_character()
                    && let Some(command_id) = self
                        .configuration
                        .settings
                        .input
                        .character_fallback_commands
                        .get(&context)
                        .cloned()
                        .or_else(|| {
                            self.configuration
                                .settings
                                .input
                                .text_commands
                                .get(&context)
                                .cloned()
                        })
                {
                    self.invoke_text_command(&command_id, character.to_string());
                    handled = true;
                }
                if !handled
                    && let Some(command_id) = self
                        .configuration
                        .settings
                        .input
                        .unmatched_commands
                        .get(&context)
                        .cloned()
                {
                    self.invoke_mapped_command(&command_id);
                    if self.active_input_context() != context {
                        self.handle_configured_key(key);
                    }
                }
            }
        }
        self.ensure_cursor_visible();
    }

    fn handle_configured_paste(&mut self, text: String) {
        let context = self.active_input_context();
        if let Some(command_id) = self
            .configuration
            .settings
            .input
            .paste_commands
            .get(&context)
            .cloned()
        {
            self.invoke_text_command(&command_id, text);
        }
    }

    fn invoke_mapped_command(&mut self, command_id: &str) {
        match command(command_id) {
            Ok(invocation) => self.invoke_mapped(invocation),
            Err(error) => self.show_error(error),
        }
    }

    fn invoke_text_command(&mut self, command_id: &str, text: String) {
        match command(command_id) {
            Ok(invocation) => {
                self.invoke(invocation.with_argument("text", CommandValue::String(text)))
            }
            Err(error) => self.show_error(error),
        }
    }

    #[cfg(test)]
    fn invoke_insert_text(&mut self, text: String) {
        self.invoke_text_command("edit.insert_text", text);
    }

    fn invoke_mapped(&mut self, mut invocation: CommandInvocation) {
        if !invocation.id.as_str().starts_with("input.") {
            self.input.decorate_invocation(&mut invocation);
        }
        self.invoke(invocation);
    }

    fn cancel_pending_mapping(&mut self) {
        if let Some((_, context)) = self.pending_keymap.take() {
            if let Some(resolver) = self.configuration.keymaps.resolvers.get_mut(&context) {
                resolver.cancel();
            }
        }
    }

    fn flush_keymap_timeout(&mut self) {
        let Some((deadline, context)) = self.pending_keymap.clone() else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.pending_keymap = None;
        if context != self.active_input_context() {
            if let Some(resolver) = self.configuration.keymaps.resolvers.get_mut(&context) {
                resolver.cancel();
            }
            return;
        }
        if let Some(resolver) = self.configuration.keymaps.resolvers.get_mut(&context) {
            if let MappingResolution::Ready(invocation) = resolver.flush_timeout() {
                self.invoke_mapped(invocation);
                self.ensure_cursor_visible();
            }
        }
    }

    fn invoke(&mut self, mut invocation: CommandInvocation) {
        if invocation.target.view.is_none() {
            invocation.target.view = self.state.active_view;
        }
        if invocation.target.buffer.is_none() {
            invocation.target.buffer = self.state.active_buffer_id();
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
        let repeat_override = visual_operator
            .then(|| self.normalized_visual_repeat(&invocation))
            .flatten();
        match self.commands.execute(&self.state, &invocation) {
            Ok(outcome) => {
                let changed = outcome.transaction.is_some();
                if let Some(mut transaction) = outcome.transaction {
                    let snippet_groups = self.prepare_snippet_mirror_transaction(&mut transaction);
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error);
                        return;
                    }
                    if let Some(groups) = snippet_groups
                        && let Some(session) = self.snippet_session.as_mut()
                    {
                        session.groups = groups;
                        session.revision = self.state.buffers[session.buffer].text.revision();
                    }
                }

                if changed {
                    self.push_change_location();
                }
                self.record_successful_invocation(
                    &invocation,
                    repeat_override.as_ref(),
                    suppress_repeat,
                );
                for effect in outcome.effects {
                    self.process_effect(effect);
                }
            }
            Err(error) => self.show_error(error),
        }
    }

    fn process_effect(&mut self, effect: AppEffect) {
        match effect {
            AppEffect::Quit { force } => {
                if !force && self.state.buffers.iter().any(|(_, buffer)| buffer.dirty) {
                    self.show_error(self.message("unsaved_quit", &[]));
                } else {
                    if force {
                        self.queue_dirty_recovery_before_exit();
                    }
                    self.should_quit = true;
                }
            }
            AppEffect::Save {
                quit_after,
                path,
                force,
            } => self.start_save(quit_after, path, force),
            AppEffect::OpenPath(path) => self.start_open(path),
            AppEffect::Reload { force } => self.start_reload(force),
            AppEffect::CloseBuffer { force } => self.close_active_buffer(force),
            AppEffect::CycleBuffer(offset) => match self.state.cycle_buffer(
                offset,
                self.configuration.settings.editor.buffer_navigation_wrap,
            ) {
                Ok(_) => {
                    self.ui.dashboard = false;
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::NewBuffer => {
                self.state.open_text(TextBuffer::from_text(""), None);
                self.ui.dashboard = false;
                self.ui.notification = Some(self.message("new_scratch", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::OpenFilePrompt => {
                let alias = self
                    .configuration
                    .settings
                    .input
                    .command_aliases
                    .iter()
                    .find(|(_, command)| command.as_str() == "file.open")
                    .map_or("file.open", |(alias, _)| alias.as_str());
                self.open_prompt(PromptKind::Command, format!("{alias} "));
            }
            AppEffect::OpenPrompt { kind, initial } => self.open_prompt(kind, initial),
            AppEffect::PromptInsert(text) => self.prompt_insert(&text),
            AppEffect::PromptBackspace => self.prompt_backspace(),
            AppEffect::PromptMove(movement) => self.prompt_move(movement),
            AppEffect::PromptHistory(direction) => self.prompt_history(direction),
            AppEffect::PromptComplete(direction) => self.prompt_complete(direction),
            AppEffect::PromptSubmit => self.submit_prompt(),
            AppEffect::PromptCancel => self.close_prompt(),
            AppEffect::SearchRepeat { reverse } => {
                let forward = if reverse {
                    !self.last_search_forward
                } else {
                    self.last_search_forward
                };
                self.perform_search(forward);
            }
            AppEffect::SearchWord { forward } => self.search_word_under_cursor(forward),
            AppEffect::Scroll {
                kind,
                count,
                at_mouse,
            } => {
                if at_mouse {
                    self.focus_mouse_pane();
                }
                self.scroll_view(&kind, count);
            }
            AppEffect::PlaceMouseCursor => self.apply_mouse_cursor(false),
            AppEffect::ExtendMouseSelection => self.apply_mouse_cursor(true),
            AppEffect::Layout(action) => self.apply_layout_action(action),
            AppEffect::RepeatLast => self.repeat_last_change(),
            AppEffect::MacroRecordStart(register) => {
                self.recording_macro = Some((register, Vec::new()));
                self.ui.notification =
                    Some(self.message("macro_recording", &[("register", register.to_string())]));
                self.ui.notification_error = false;
            }
            AppEffect::MacroRecordStop => {
                if let Some((register, invocations)) = self.recording_macro.take() {
                    self.state.registers.set_macro(register, invocations);
                    self.ui.notification =
                        Some(self.message("macro_recorded", &[("register", register.to_string())]));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.message("no_macro_recording", &[]));
                }
            }
            AppEffect::MacroReplay { register, count } => {
                if self.replay_depth == 0 {
                    self.replay_steps = 0;
                }
                for _ in 0..count {
                    self.replay_macro(register);
                    if self.ui.notification_error {
                        break;
                    }
                }
            }
            AppEffect::Grammar(input) => self.process_grammar_input(input),
            AppEffect::CancelPendingMapping => self.cancel_pending_mapping(),
            AppEffect::ReloadConfiguration => match self.configuration_worker.request_reload() {
                Ok(()) => {
                    self.ui.notification = Some(self.message("config_reload_queued", &[]));
                    self.ui.notification_error = false;
                }
                Err(error) => self.show_error(error),
            },
            AppEffect::ExplainConfig(key) => {
                if let Some(explanation) = self.configuration.explain(&key) {
                    self.ui.notification = Some(self.message(
                        "config_explanation",
                        &[
                            ("key", explanation.key),
                            ("value", explanation.value),
                            ("source", explanation.source),
                        ],
                    ));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.message("unknown_config_key", &[("key", key)]));
                }
            }
            AppEffect::InspectKeymap {
                context,
                key,
                command,
            } => {
                let context = context.unwrap_or_else(|| self.active_input_context());
                let inspection = key
                    .as_deref()
                    .and_then(|keys| self.configuration.keymaps.inspect_key(&context, keys))
                    .or_else(|| {
                        command.as_deref().and_then(|command| {
                            self.configuration.keymaps.inspect(&context, command)
                        })
                    });
                if let Some(inspection) = inspection {
                    self.ui.notification = Some(self.message(
                        "keymap_inspection",
                        &[
                            ("context", inspection.context),
                            ("keys", inspection.keys),
                            ("command", inspection.command),
                            ("arguments", format!("{:?}", inspection.arguments)),
                            ("source", inspection.source),
                        ],
                    ));
                    self.ui.notification_error = false;
                } else if command.is_none() && key.is_none() {
                    let count = self
                        .configuration
                        .keymaps
                        .reverse
                        .keys()
                        .filter(|(mapped_context, _)| mapped_context == &context)
                        .count();
                    self.ui.notification = Some(self.message(
                        "keymap_context_summary",
                        &[("context", context), ("count", count.to_string())],
                    ));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.message("no_keymap_binding", &[("context", context)]));
                }
            }
            AppEffect::ShowKeymapConflicts => {
                if self.configuration.keymaps.conflicts.is_empty() {
                    self.ui.notification = Some(self.message("no_keymap_conflicts", &[]));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.configuration.keymaps.conflicts.join("; "));
                }
            }
            AppEffect::RecoveryInspect => {
                self.ui.notification = Some(self.message(
                    "recovery_summary",
                    &[("count", self.recovery_records.len().to_string())],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::RecoveryRecoverLatest => {
                if let Some((_, record)) = self.recovery_records.last() {
                    self.state
                        .open_text(TextBuffer::from_text(&record.content), None);
                    self.ui.dashboard = false;
                    self.ui.notification = Some(self.message("recovery_opened", &[]));
                    self.ui.notification_error = false;
                } else {
                    self.show_error(self.message("recovery_none", &[]));
                }
            }
            AppEffect::RecoveryDiscardLatest => {
                if let Some((path, _)) = self.recovery_records.last() {
                    let path = path.clone();
                    match self.vfs.delete(path.clone()) {
                        Ok(id) => {
                            self.pending_vfs
                                .insert(id, PendingVfs::RecoveryDelete { path, buffer: None });
                        }
                        Err(error) => self.show_error(error),
                    }
                } else {
                    self.show_error(self.message("recovery_none", &[]));
                }
            }
            AppEffect::SessionSave => {
                self.save_persistence();
                self.ui.notification = Some(self.message("session_save_queued", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::SessionLoad => {
                self.load_persistence();
                self.ui.notification = Some(self.message("session_load_queued", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::HistoryClear => {
                self.command_history.clear();
                self.search_history.clear();
                self.ui.notification = Some(self.message("history_cleared", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::LargeFileSet(mode) => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return;
                };
                let mode = match mode.as_str() {
                    "auto" => LargeFileOverride::Auto,
                    "on" => LargeFileOverride::On,
                    "off" => LargeFileOverride::Off,
                    _ => {
                        self.show_error(self.message("large_file_invalid", &[]));
                        return;
                    }
                };
                self.large_file_overrides.insert(buffer, mode);
                if let Some(path) = self.state.buffers[buffer].path.clone() {
                    if mode == LargeFileOverride::Auto {
                        self.persisted_large_file_overrides.remove(&path);
                    } else {
                        self.persisted_large_file_overrides.insert(path, mode);
                    }
                }
                self.process_large_file_modes();
                self.ui.notification = Some(self.message(
                    "large_file_changed",
                    &[("mode", mode_name_large(mode).into())],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::LargeFileStatus => {
                let Some(buffer) = self
                    .state
                    .active_view
                    .and_then(|view| self.state.views.get(view).map(|view| view.buffer))
                else {
                    return;
                };
                let configured = self
                    .large_file_overrides
                    .get(&buffer)
                    .copied()
                    .unwrap_or(LargeFileOverride::Auto);
                let effective = if self.effective_large_file(buffer) {
                    "on"
                } else {
                    "off"
                };
                self.ui.notification = Some(self.message(
                    "large_file_status",
                    &[
                        ("mode", mode_name_large(configured).into()),
                        ("effective", effective.into()),
                    ],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::SyntaxFoldToggle => {
                if let Some(view) = self.state.active_view {
                    let line = self.state.views.get(view).and_then(|view| {
                        self.state.buffers[view.buffer]
                            .text
                            .byte_to_line(view.cursor)
                            .ok()
                    });
                    if let Some(fold) = self.ui.syntax_folds.iter_mut().find(|fold| {
                        fold.view == view
                            && line.is_some_and(|line| {
                                line >= fold.start_line && line <= fold.end_line
                            })
                    }) {
                        fold.collapsed = !fold.collapsed;
                    }
                }
            }
            AppEffect::SyntaxIndentLine => self.apply_syntax_indent_line(),
            AppEffect::SyntaxStatus => {
                let active = self.state.active_buffer_id();
                let active_language = active
                    .and_then(|buffer| self.state.buffers.get(buffer))
                    .and_then(|buffer| buffer.path.as_deref())
                    .and_then(language_for_path);
                let items = builtin_languages()
                    .enumerate()
                    .map(|(index, info)| {
                        let state = if Some(info.language) == active_language {
                            if let Some(buffer) = active {
                                if let Some(error) = self.syntax_failures.get(&buffer) {
                                    self.message(
                                        "syntax_state_failed",
                                        &[("error", error.diagnostic.clone())],
                                    )
                                } else if self.syntax_accepted.get(&buffer).is_some_and(|result| {
                                    self.state.buffers.get(buffer).is_some_and(|state| {
                                        state.text.revision().0 == result.revision
                                    }) && result.language == info.language
                                }) {
                                    self.message(
                                        if self.syntax_accepted[&buffer].truncated {
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
                                id: PickerItemId(format!("syntax:{index}:{}", info.id)),
                                label: self.syntax_language_label(info.language),
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
                let Some(task) = self.configuration.settings.tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return;
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("test_matcher_missing", &[]));
                    return;
                };
                self.defer_trusted_execution(DeferredExecution::RunTest { name, matcher });
            }
            AppEffect::TestStop(name) => {
                let job = self
                    .job_tasks
                    .iter()
                    .find_map(|(job, task)| (task.requested == name).then_some(*job));
                if let Some(job) = job {
                    let _ = self.jobs.cancel(job);
                    self.task_status.insert(name, "cancelled".into());
                }
            }
            AppEffect::TestList(name) => self.show_test_picker(&name),
            AppEffect::TaskList => self.show_task_picker(),
            AppEffect::TaskRestart(name) => {
                let job = self
                    .job_tasks
                    .iter()
                    .find_map(|(job, task)| (task.requested == name).then_some(*job));
                if let Some(job) = job {
                    let _ = self.jobs.cancel(job);
                }
                self.defer_trusted_execution(DeferredExecution::RunTask(name));
            }
            AppEffect::LintRun(name) => {
                let Some(task) = self.configuration.settings.tasks.get(&name) else {
                    self.show_error(self.message("task_unknown", &[("name", name)]));
                    return;
                };
                let Some(matcher) = task.matcher.clone() else {
                    self.show_error(self.message("linter_matcher_missing", &[]));
                    return;
                };
                self.defer_trusted_execution(DeferredExecution::RunLinter { name, matcher });
            }
            AppEffect::LintClear(name) => {
                if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                    self.diagnostics.clear_owner(&owner);
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::FormatRun(name) => {
                let Some(view_id) = self.state.active_view else {
                    return;
                };
                let Some(view) = self.state.views.get(view_id) else {
                    return;
                };
                let buffer = view.buffer;
                let state = &self.state.buffers[buffer];
                let source = state.text.to_bytes().to_vec();
                self.defer_trusted_execution(DeferredExecution::RunFormatter {
                    name,
                    buffer,
                    revision: state.text.revision(),
                    source,
                });
            }
            AppEffect::TaskRun(name) => {
                self.defer_trusted_execution(DeferredExecution::RunTask(name))
            }
            AppEffect::TaskStop(name) => {
                let job = self
                    .job_tasks
                    .iter()
                    .find_map(|(job, task)| (task.requested == name).then_some(*job));
                if let Some(job) = job {
                    if let Err(error) = self.jobs.cancel(job) {
                        self.show_error(error)
                    }
                } else {
                    self.show_error(self.message("task_not_running", &[("name", name)]));
                }
            }
            AppEffect::TaskOutput(name) => self.show_output_picker(&name),
            AppEffect::CrashInspect => {
                let path = crate::configuration::user_state_directory().join("last-crash.json");
                self.ui.notification =
                    Some(self.message("crash_location", &[("path", path.display().to_string())]));
                self.ui.notification_error = path.is_file();
            }
            AppEffect::CrashClear => {
                let path = crate::configuration::user_state_directory().join("last-crash.json");
                match self.vfs.delete(path) {
                    Ok(id) => {
                        self.pending_vfs.insert(id, PendingVfs::CrashDelete);
                    }
                    Err(error) => self.show_error(error),
                }
            }
            AppEffect::TerminalOpen(profile) => {
                self.defer_trusted_execution(DeferredExecution::OpenTerminal(profile))
            }
            AppEffect::TerminalList => {
                let status = self
                    .terminals
                    .iter()
                    .map(|(id, terminal)| {
                        format!(
                            "#{} {} {}",
                            id.0,
                            terminal.profile,
                            if terminal.running {
                                "running"
                            } else {
                                "exited"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.ui.notification = Some(self.message("terminal_list", &[("status", status)]));
                self.ui.notification_error = false;
            }
            AppEffect::TerminalFocus(id) => {
                let id = PtyId(id);
                if self.terminals.contains_key(&id) {
                    self.active_terminal = Some(id);
                    self.sync_terminal_panel();
                } else {
                    self.show_error(self.message("terminal_missing", &[]));
                }
            }
            AppEffect::TerminalUnfocus => {
                self.active_terminal = None;
                self.ui.terminal_panel = None;
            }
            AppEffect::TerminalSend(text) => {
                if let Some(id) = self.active_terminal {
                    if let Err(error) = self.ptys.input(
                        id,
                        text.into_bytes(),
                        self.configuration.settings.terminals.maximum_input_bytes,
                    ) {
                        self.show_error(error)
                    }
                }
            }
            AppEffect::TerminalClose => {
                if let Some(id) = self.active_terminal.take() {
                    let _ = self.ptys.close(id);
                    self.ui.terminal_panel = None;
                }
            }
            AppEffect::PluginValidate => self.load_configured_plugins(),
            AppEffect::PluginList => {
                let items = self
                    .plugins
                    .statuses
                    .iter()
                    .take(4096)
                    .enumerate()
                    .map(|(index, (id, status))| {
                        (
                            PickerItem {
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
                self.install_picker(self.message("plugin_picker_title", &[]), items);
            }
            AppEffect::PluginDisable(id) => {
                if let Err(error) = self.plugins.disable(&id) {
                    self.show_error(error)
                } else {
                    self.commands.unregister_plugin(&id);
                    self.ui.notification = Some(self.message("plugin_disabled", &[("id", id)]));
                    self.ui.notification_error = false;
                }
            }
            AppEffect::PluginExecute { plugin, command } => {
                let snapshot = self.state.active_buffer_id().and_then(|id| {
                    self.state.buffers.get(id).map(|buffer| PluginSnapshot {
                        buffer: id.data().as_ffi(),
                        revision: buffer.text.revision().0,
                        utf8: buffer.text.to_string(),
                    })
                });
                if let Err(error) = self.plugins.execute(
                    &plugin,
                    &command,
                    Vec::new(),
                    snapshot,
                    BTreeMap::new(),
                    self.configuration_generation,
                ) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageInstall {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => {
                if self.safe_mode {
                    self.show_error(self.message("safe_mode_blocked", &[]));
                } else if !self.execution_revalidated {
                    self.defer_trusted_execution(DeferredExecution::InstallPackage {
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    });
                } else {
                    self.install_package_after_validation(
                        package_manifest,
                        plugin_manifest,
                        component,
                        update,
                    )
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
                if self.safe_mode {
                    self.show_error(self.message("safe_mode_blocked", &[]))
                } else if !self.execution_revalidated {
                    self.defer_trusted_execution(DeferredExecution::InstallRegistry {
                        publisher,
                        name,
                        version,
                    })
                } else {
                    self.request_registry(
                        RegistryRequest::Show {
                            publisher,
                            name,
                            version,
                        },
                        true,
                    )
                }
            }
            AppEffect::PackageList => {
                if let Err(error) = self.packages.request(PackageOperation::List) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageVerify => {
                if let Err(error) = self.packages.request(PackageOperation::Verify) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRollback => {
                self.package_startup_rollback_attempted = false;
                if let Err(error) = self.packages.request(PackageOperation::Rollback) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageRemove(name) => {
                self.package_startup_rollback_attempted = false;
                if let Err(error) = self.packages.request(PackageOperation::Remove(name)) {
                    self.show_error(error)
                }
            }
            AppEffect::PackageGarbageCollect => {
                if let Err(error) = self
                    .packages
                    .request(PackageOperation::GarbageCollect { maximum: 256 })
                {
                    self.show_error(error)
                }
            }
            AppEffect::WorkspaceTrust => self.request_workspace_trust(),
            AppEffect::WorkspaceTrustConfirm(phrase) => {
                self.confirm_workspace_trust(phrase);
            }
            AppEffect::WorkspaceUntrust => self.revoke_workspace_trust(),
            AppEffect::WorkspaceStatus => {
                let state = if self.workspace.trust() == WorkspaceTrust::Trusted {
                    self.message("workspace_trust_trusted", &[])
                } else {
                    self.message("workspace_trust_untrusted", &[])
                };
                self.ui.notification =
                    Some(self.message("workspace_trust_status", &[("state", state)]));
                self.ui.notification_error = false;
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerClose => {
                self.ui.explorer = None;
                self.ui.explorer_focused = false;
            }
            AppEffect::ExplorerMove(delta) => {
                self.explorer.move_selection(isize::from(delta));
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerParent => {
                self.explorer.collapse_or_parent();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerChild => {
                self.explorer.select_first_child();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerToggle => self.explorer_toggle(),
            AppEffect::ExplorerRefresh => self.refresh_explorer(),
            AppEffect::ExplorerToggleHidden => {
                self.explorer_hidden = !self.explorer_hidden;
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer_ignored = !self.explorer_ignored;
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
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
                    self.ui.notification = Some(self.message(
                        "workspace_root_status",
                        &[
                            ("id", root.id.0.to_string()),
                            ("path", root.identity.canonical_path.display().to_string()),
                        ],
                    ));
                    self.ui.notification_error = false;
                }
            }
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.workspace_search.cancel();
                self.workspace_search_generation = None;
                self.workspace_search_intent = None;
                self.workspace_search_matches.clear();
                self.ui.notification = Some(self.message("workspace_search_cancelled", &[]));
                self.ui.notification_error = false;
            }
            AppEffect::WorkspaceReplace { query, replacement } => {
                self.start_workspace_search(query, Some(replacement));
            }
            AppEffect::OpenPicker(kind) => self.open_builtin_picker(kind),
            AppEffect::PickerInsert(text) => self.picker_insert(&text),
            AppEffect::PickerBackspace => self.picker_backspace(),
            AppEffect::PickerMove(direction) => self.picker_move(direction),
            AppEffect::PickerAccept => self.accept_picker(),
            AppEffect::PickerCancel => self.close_picker(),
            AppEffect::WorkspaceEditToggle => {
                let selected = self
                    .picker
                    .as_ref()
                    .and_then(PickerState::selected_item)
                    .and_then(|item| self.picker_payloads.get(&item.id))
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected
                    && let Some(proposal) = &mut self.workspace_proposal
                    && proposal.toggle_file(index).is_ok()
                {
                    self.show_workspace_proposal();
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => {
                let intents = std::mem::take(&mut self.accepted_resource_intents);
                for intent in intents {
                    if self.state.buffers.iter().any(|(_, buffer)| {
                        buffer.path.as_ref() == Some(&intent.source)
                            && (buffer.dirty || buffer.text.to_bytes() != intent.source_bytes)
                    }) {
                        self.show_error(self.message(
                            "workspace_resource_buffer_stale",
                            &[("path", intent.source.display().to_string())],
                        ));
                        continue;
                    }
                    match self.vfs.apply_resource(intent.clone()) {
                        Ok(id) => {
                            self.resource_apply_pending.insert(id, intent);
                        }
                        Err(error) => self.show_error(error),
                    }
                }
            }
            AppEffect::OpenToolMenu(category) => self.open_tool_menu(category),
            AppEffect::CloseToolMenu => self.ui.tool_menu = None,
            AppEffect::MoveToolMenu(direction) => self.move_tool_menu(direction),
            AppEffect::CycleToolCategory(direction) => self.cycle_tool_category(direction),
            AppEffect::RefreshTools => self.refresh_tools(),
            AppEffect::ActivateTool => self.activate_selected_tool(),
            AppEffect::LspStart => {
                if let Some(key) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&key) {
                        state.restart_count = 0;
                        state.restart_due = None;
                        state.circuit_open = false;
                        state.desired_running = true;
                    }
                }
                self.start_lsp(None);
            }
            AppEffect::LspStop => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&server) {
                        state.desired_running = false;
                        state.restart_due = None;
                    }
                    if let Err(error) = self.lsp.stop(&server) {
                        self.show_error(error)
                    }
                    self.lsp_sessions.remove(&server);
                    self.clear_lsp_session_state();
                }
            }
            AppEffect::LspRestart => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some(state) = self.lsp_sessions.get_mut(&server) {
                        state.restart_count = 0;
                        state.restart_due = Some(Instant::now());
                        state.circuit_open = false;
                        state.desired_running = true;
                    }
                    let _ = self.lsp.stop(&server);
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspStatus => {
                let servers = self.lsp.running_servers();
                self.ui.notification = Some(self.message(
                    "lsp_status",
                    &[(
                        "servers",
                        if servers.is_empty() {
                            "none".into()
                        } else {
                            servers.join(", ")
                        },
                    )],
                ));
                self.ui.notification_error = false;
                if let Some(server) = self.active_lsp_server.as_deref() {
                    let _ = self.lsp.health(server);
                }
            }
            AppEffect::LspAttach => {
                if let Some(server) = self.active_lsp_server.clone() {
                    if let Some((buffer, document)) = self.active_lsp_document() {
                        let revision = BufferRevision(document.version);
                        if let Err(error) = self.lsp.sync(&server, document) {
                            self.show_error(error);
                        } else {
                            self.lsp_synced_revisions.insert(buffer, revision);
                        }
                    }
                } else {
                    self.start_lsp(None);
                }
            }
            AppEffect::LspDetach => {
                if let (Some(server), Some((buffer, document))) = (
                    self.active_lsp_server.as_deref(),
                    self.active_lsp_document(),
                ) {
                    let uri = document.uri();
                    let _ = self.lsp.close(server, uri.clone());
                    self.lsp_synced_revisions.remove(&buffer);
                    if let Ok(owner) = DiagnosticOwner::new(format!("lsp.{server}")) {
                        self.diagnostics.clear_owner_uri(&owner, &uri);
                    }
                    self.sync_ui_diagnostics();
                }
            }
            AppEffect::LspSessions => self.show_lsp_sessions(),
            AppEffect::LspSessionSelect(key) => self.select_lsp_session(&key),
            AppEffect::LspLogs => {
                let logs = self
                    .lsp_logs
                    .iter()
                    .rev()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ");
                self.ui.notification = Some(self.message("lsp_logs", &[("logs", logs)]));
                self.ui.notification_error = false;
            }
            AppEffect::LspRequest(feature) => self.request_lsp(feature),
            AppEffect::LspCompletionMove(direction) => self.move_lsp_completion(direction),
            AppEffect::DiagnosticNavigate(direction) => self.navigate_diagnostic(direction),
            AppEffect::LspCompletionAccept => self.accept_lsp_completion(),
            AppEffect::LspCompletionCancel => {
                let generation = self.lsp_request_generation;
                if let Some(server) = self.active_lsp_server.as_deref() {
                    let _ = self.lsp.cancel(server, generation);
                }
                self.lsp_completion_items.clear();
                self.ui.language_completion = None;
            }
            AppEffect::SnippetNavigate(direction) => self.navigate_snippet(direction),
            AppEffect::SnippetCancel => {
                self.snippet_session = None;
                if let Some(view) = self.state.active_view {
                    self.state.views[view].selection = None;
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
                    if let Some(fold) = self.ui.lsp_folds.iter_mut().find(|fold| {
                        fold.view == view
                            && line.is_some_and(|line| {
                                line >= fold.start_line && line <= fold.end_line
                            })
                    }) {
                        fold.collapsed = !fold.collapsed;
                    }
                }
            }
            AppEffect::LspSelectionExpand => {
                if let (Some(view), Some(range)) = (
                    self.state.active_view,
                    self.lsp_selection_ranges
                        .iter()
                        .filter(|range| {
                            range.contains(self.state.views[self.state.active_view.unwrap()].cursor)
                        })
                        .min_by_key(|range| range.len())
                        .copied(),
                ) {
                    let transaction = EditorTransaction::new(self.transaction_id()).with_operation(
                        EditorOperation::SetSelection {
                            view,
                            selection: Some(crate::editing::Selection {
                                anchor: range.start,
                                head: range.end,
                                kind: crate::editing::SelectionKind::Character,
                            }),
                        },
                    );
                    if let Err(error) = self.state.apply_transaction(&transaction) {
                        self.show_error(error)
                    }
                }
            }
            AppEffect::DapStart(program) => self.start_dap(None, program),
            AppEffect::DapConfigurationStart(name) => self.start_dap_configuration(&name),
            AppEffect::DapWatchAdd(expression) => {
                if !expression.is_empty()
                    && expression.len() <= 64 * 1024
                    && self.dap_watches.len() < self.configuration.settings.debug.maximum_watches
                    && !self.dap_watches.contains(&expression)
                {
                    self.dap_watches.push(expression);
                }
            }
            AppEffect::DapWatchRemove(expression) => {
                self.dap_watches.retain(|watch| watch != &expression)
            }
            AppEffect::DapWatchList => self.open_dap_picker(
                "watches",
                self.dap_watches
                    .iter()
                    .cloned()
                    .map(|expression| {
                        (
                            expression.clone(),
                            String::new(),
                            DapControl::Evaluate {
                                expression,
                                frame_id: None,
                                context: "watch".into(),
                            },
                        )
                    })
                    .collect(),
            ),
            AppEffect::DapStop => {
                if let Err(error) = self.dap.stop() {
                    self.show_error(error);
                }
            }
            AppEffect::DapContinue => {
                if let Err(error) = self.dap.control(DapControl::Continue) {
                    self.show_error(error);
                }
            }
            AppEffect::DapPause => {
                if let Err(error) = self.dap.control(DapControl::Pause) {
                    self.show_error(error);
                }
            }
            AppEffect::DapNext => {
                if let Err(error) = self.dap.control(DapControl::Next) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepIn => {
                if let Err(error) = self.dap.control(DapControl::StepIn) {
                    self.show_error(error);
                }
            }
            AppEffect::DapStepOut => {
                if let Err(error) = self.dap.control(DapControl::StepOut) {
                    self.show_error(error);
                }
            }
            AppEffect::DapConsole => {
                let items = self
                    .dap_console
                    .iter()
                    .enumerate()
                    .map(|(index, (adapter, line))| {
                        (
                            PickerItem {
                                id: PickerItemId(format!("dap-console:{index}")),
                                label: line.clone(),
                                detail: String::new(),
                                source: adapter.clone(),
                                action: PickerAction::None,
                            },
                            PickerPayload::Command(String::new()),
                        )
                    })
                    .collect();
                self.install_picker(self.message("dap_console_title", &[]), items);
            }
            AppEffect::DapSessions => {
                let adapter = self.dap.active_adapter().unwrap_or("none").to_owned();
                let configuration = self.active_debug_configuration.clone().unwrap_or_default();
                let items = vec![(
                    PickerItem {
                        id: PickerItemId("dap-session:active".into()),
                        label: adapter,
                        detail: configuration,
                        source: if self.dap.is_running() {
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
                if let Err(error) = self.dap.control(DapControl::Threads) {
                    self.show_error(error)
                }
            }
            AppEffect::DapStack(thread) => {
                if let Err(error) = self.dap.control(DapControl::StackTrace {
                    thread_id: thread,
                    start: 0,
                    levels: 256,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapScopes(frame) => {
                if let Err(error) = self.dap.control(DapControl::Scopes { frame_id: frame }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapVariables(reference) => {
                if let Err(error) = self.dap.control(DapControl::Variables {
                    reference,
                    start: 0,
                    count: 1000,
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapEvaluate(expression) => {
                if let Err(error) = self.dap.control(DapControl::Evaluate {
                    expression,
                    frame_id: None,
                    context: "repl".into(),
                }) {
                    self.show_error(error)
                }
            }
            AppEffect::DapRestart => {
                if let Some(name) = self.active_debug_configuration.clone() {
                    let _ = self.dap.stop();
                    self.start_dap_configuration(&name)
                } else {
                    self.show_error(self.message("dap_no_configuration", &[]));
                }
            }
            AppEffect::DapBreakpointConfigure {
                condition,
                hit_condition,
                log_message,
            } => {
                let Some(view) = self.state.active_view else {
                    return;
                };
                let buffer = &self.state.buffers[self.state.views[view].buffer];
                let Some(path) = buffer.path.clone() else {
                    return;
                };
                let Ok(line) = buffer
                    .text
                    .byte_to_line(self.state.views[view].cursor)
                    .map(|line| line as u64 + 1)
                else {
                    return;
                };
                for value in [
                    condition.as_ref(),
                    hit_condition.as_ref(),
                    log_message.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if value.len() > 64 * 1024 || value.chars().any(|character| character == '\0') {
                        self.show_error(self.message("dap_breakpoint_invalid", &[]));
                        return;
                    }
                }
                let rows = self.dap_breakpoints.entry(path.clone()).or_default();
                if let Some(row) = rows.iter_mut().find(|row| row.line == line) {
                    row.condition = condition;
                    row.hit_condition = hit_condition;
                    row.log_message = log_message;
                } else if rows.len() < self.configuration.settings.debug.maximum_breakpoints {
                    rows.push(DapSourceBreakpoint {
                        path: path.clone(),
                        line,
                        column: None,
                        condition,
                        hit_condition,
                        log_message,
                    });
                }
                if self.dap.is_running() {
                    let _ = self.dap.control(DapControl::SetBreakpoints {
                        path,
                        breakpoints: rows.clone(),
                    });
                }
            }
            AppEffect::DapBreakpointToggle => {
                let Some(view) = self.state.active_view else {
                    return;
                };
                let buffer = &self.state.buffers[self.state.views[view].buffer];
                let Some(path) = buffer.path.clone() else {
                    return;
                };
                let Ok(line) = buffer
                    .text
                    .byte_to_line(self.state.views[view].cursor)
                    .map(|line| line as u64 + 1)
                else {
                    return;
                };
                let rows = self.dap_breakpoints.entry(path.clone()).or_default();
                let removed = rows.iter().any(|row| row.line == line);
                if removed {
                    rows.retain(|row| row.line != line)
                } else {
                    rows.push(DapSourceBreakpoint {
                        path: path.clone(),
                        line,
                        column: None,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    })
                }
                let breakpoints = rows.clone();
                if self.dap.is_running() {
                    let _ = self.dap.control(DapControl::SetBreakpoints {
                        path: path.clone(),
                        breakpoints,
                    });
                }
                self.ui.notification = Some(self.message(
                    "dap_breakpoint_status",
                    &[
                        (
                            "state",
                            if removed {
                                "removed".into()
                            } else {
                                "added".into()
                            },
                        ),
                        ("path", path.display().to_string()),
                        ("line", line.to_string()),
                    ],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::DapStatus => {
                self.ui.notification = Some(self.message(
                    "dap_status",
                    &[(
                        "adapter",
                        self.dap.active_adapter().unwrap_or("none").to_owned(),
                    )],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::MarkSet(name) => {
                if name.len() > 64 || name.chars().any(char::is_control) {
                    return;
                }
                if let Some(location) = self.current_persisted_location() {
                    self.marks.retain(|mark| mark.name != name);
                    self.marks.push(PersistedMark { name, location });
                    let maximum = self.configuration.settings.persistence.marks;
                    if self.marks.len() > maximum {
                        self.marks.drain(..self.marks.len() - maximum);
                    }
                }
            }
            AppEffect::SafeModeStatus => {
                self.ui.notification = Some(self.message(
                    "safe_mode_status",
                    &[(
                        "state",
                        if self.safe_mode {
                            "active".into()
                        } else {
                            "inactive".into()
                        },
                    )],
                ));
                self.ui.notification_error = false;
            }
            AppEffect::ShowHealth => {
                self.ui.notification = Some(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.terminal_capabilities.clone()),
                            ("jobs", self.job_tasks.len().to_string()),
                            (
                                "ptys",
                                self.terminals
                                    .values()
                                    .filter(|terminal| terminal.running)
                                    .count()
                                    .to_string(),
                            ),
                            (
                                "preflights",
                                self.pending_execution_preflight.len().to_string(),
                            ),
                            ("tasks", self.task_status.len().to_string()),
                            ("plugins", self.plugins.statuses.len().to_string()),
                            (
                                "plugin_failures",
                                self.plugins
                                    .statuses
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::plugins::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.packages.pending_count().to_string()),
                            ("lsp", self.lsp_sessions.len().to_string()),
                            (
                                "persistence",
                                self.persistence_pending.is_some().to_string(),
                            ),
                            ("safe", self.safe_mode.to_string()),
                            ("dap", self.dap.is_running().to_string()),
                        ],
                    ),
                );
                self.ui.notification_error = false;
            }
        }
    }

    fn process_grammar_input(&mut self, input: crate::input::GrammarInput) {
        let mode = self
            .state
            .active_view
            .and_then(|view| self.state.views.get(view))
            .map_or(EditorMode::Normal, |view| view.mode);
        match self.input.feed(mode, input) {
            Ok(InputAction::Invoke(invocation)) => self.invoke_mapped(invocation),
            Ok(InputAction::Pending) => {
                if self.input.effective_mode(mode) == EditorMode::OperatorPending
                    && mode != EditorMode::OperatorPending
                {
                    self.set_mode(EditorMode::OperatorPending);
                }
            }
            Ok(InputAction::Cancelled) => {
                if mode == EditorMode::OperatorPending {
                    self.set_mode(EditorMode::Normal);
                }
            }
            Ok(InputAction::Text(text)) => {
                let context = self.active_input_context();
                if let Some(command_id) = self
                    .configuration
                    .settings
                    .input
                    .text_commands
                    .get(&context)
                    .cloned()
                {
                    self.invoke_text_command(&command_id, text);
                }
            }
            Ok(InputAction::Unmapped) => {}
            Err(error) => self.show_error(error),
        }
    }

    fn normalized_visual_repeat(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<CommandInvocation> {
        let view_id = invocation.target.view.or(self.state.active_view)?;
        let view = self.state.views.get(view_id)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        let selection = view.selection?;
        let (shape, width, height) = match selection.kind {
            crate::editing::SelectionKind::Character => {
                let low = selection.anchor.min(selection.head);
                let high = selection.anchor.max(selection.head);
                let end = next_grapheme_boundary(&buffer.text, high);
                let selected = buffer.text.slice(ByteRange::new(low, end).ok()?).ok()?;
                ("character", selected.graphemes(true).count(), 1_usize)
            }
            crate::editing::SelectionKind::Line => {
                let anchor = buffer.text.byte_to_line(selection.anchor).ok()?;
                let head = buffer.text.byte_to_line(selection.head).ok()?;
                ("line", 1, anchor.abs_diff(head).saturating_add(1))
            }
            crate::editing::SelectionKind::Block => {
                let anchor_line = buffer.text.byte_to_line(selection.anchor).ok()?;
                let head_line = buffer.text.byte_to_line(selection.head).ok()?;
                let anchor_start = buffer.text.line_to_byte(anchor_line).ok()?;
                let head_start = buffer.text.line_to_byte(head_line).ok()?;
                let anchor_text = buffer.text.line(anchor_line).ok()?;
                let head_text = buffer.text.line(head_line).ok()?;
                let width_policy = DisplayWidthPolicy {
                    tab_width: self.state.policy.tab_width,
                    ambiguous_wide: self.state.policy.ambiguous_width_wide,
                };
                let anchor_column = display_column_at(
                    anchor_text.strip_suffix('\n').unwrap_or(&anchor_text),
                    selection.anchor.get().saturating_sub(anchor_start.get()),
                    width_policy,
                );
                let head_column = display_column_at(
                    head_text.strip_suffix('\n').unwrap_or(&head_text),
                    selection.head.get().saturating_sub(head_start.get()),
                    width_policy,
                );
                (
                    "block",
                    anchor_column.abs_diff(head_column).saturating_add(1),
                    anchor_line.abs_diff(head_line).saturating_add(1),
                )
            }
        };
        let operator = invocation.arguments.get("operator")?.clone();
        let mut repeat = CommandInvocation::new("edit.repeat_visual", invocation.source).ok()?;
        repeat.arguments.insert("operator".into(), operator);
        repeat
            .arguments
            .insert("shape".into(), CommandValue::String(shape.into()));
        repeat.arguments.insert(
            "width".into(),
            CommandValue::Integer(i64::try_from(width).ok()?),
        );
        repeat.arguments.insert(
            "height".into(),
            CommandValue::Integer(i64::try_from(height).ok()?),
        );
        repeat.register = invocation.register;
        repeat.count = invocation.count;
        Some(repeat)
    }

    fn record_successful_invocation(
        &mut self,
        invocation: &CommandInvocation,
        repeat_override: Option<&CommandInvocation>,
        suppress_repeat: bool,
    ) {
        if !invocation.id.as_str().starts_with("macro.")
            && !invocation.id.as_str().starts_with("input.")
        {
            if let Some((_, recorded)) = &mut self.recording_macro {
                let mut invocation = invocation.clone();
                invocation.target = Default::default();
                recorded.push(invocation);
            }
        }
        if invocation.source == InvocationSource::Macro {
            return;
        }
        if suppress_repeat {
            return;
        }
        let mut semantic = repeat_override.unwrap_or(invocation).clone();
        semantic.target = Default::default();
        match invocation.id.as_str() {
            "mode.insert" | "mode.replace" | "edit.substitute" => {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.operator" | "edit.repeat_visual"
                if semantic
                    .arguments
                    .get("operator")
                    .is_some_and(|value| value == &CommandValue::String("change".into())) =>
            {
                self.pending_change = Some(vec![semantic]);
            }
            "edit.insert_text"
            | "edit.backspace"
            | "edit.replace_text"
            | "edit.replace_backspace" => {
                if let Some(pending) = &mut self.pending_change {
                    pending.push(semantic);
                }
            }
            "mode.normal" => {
                if let Some(mut pending) = self.pending_change.take() {
                    pending.push(semantic);
                    self.last_change = pending;
                }
            }
            "edit.delete_char"
            | "edit.delete_backward"
            | "edit.replace"
            | "edit.join_lines"
            | "edit.toggle_case"
            | "edit.put" => self.last_change = vec![semantic],
            "edit.operator" | "edit.repeat_visual" => self.last_change = vec![semantic],
            _ => {}
        }
    }

    fn repeat_last_change(&mut self) {
        if self.last_change.is_empty() {
            self.show_error(self.message("no_previous_change", &[]));
            return;
        }
        if self.last_change.len() > self.configuration.settings.runtime.repeat_step_limit {
            self.show_error(
                self.message(
                    "repeat_step_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings
                            .runtime
                            .repeat_step_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        let invocations = self.last_change.clone();
        for mut invocation in invocations {
            invocation.source = InvocationSource::Macro;
            invocation.target = Default::default();
            self.invoke(invocation);
        }
    }

    fn replay_macro(&mut self, requested: char) {
        let register = if requested == '@' {
            let Some(register) = self.last_macro else {
                self.show_error(self.message("no_previous_macro", &[]));
                return;
            };
            register
        } else {
            requested
        };
        let Some(invocations) = self.state.registers.get_macro(register).map(<[_]>::to_vec) else {
            self.show_error(self.message("empty_macro", &[("register", register.to_string())]));
            return;
        };
        if self.replay_depth >= self.configuration.settings.runtime.macro_recursion_limit {
            self.show_error(
                self.message(
                    "macro_recursion_limit",
                    &[(
                        "limit",
                        self.configuration
                            .settings
                            .runtime
                            .macro_recursion_limit
                            .to_string(),
                    )],
                ),
            );
            return;
        }
        self.last_macro = Some(register);
        self.replay_depth += 1;
        for mut invocation in invocations {
            if self.replay_steps >= self.configuration.settings.runtime.macro_step_limit {
                self.show_error(
                    self.message(
                        "macro_step_limit",
                        &[(
                            "limit",
                            self.configuration
                                .settings
                                .runtime
                                .macro_step_limit
                                .to_string(),
                        )],
                    ),
                );
                break;
            }
            self.replay_steps += 1;
            invocation.source = InvocationSource::Macro;
            invocation.target = Default::default();
            self.invoke(invocation);
        }
        self.replay_depth -= 1;
    }

    fn search_word_under_cursor(&mut self, forward: bool) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let source = self.state.buffers[view.buffer].text.to_string();
        let cursor = view.cursor.get();
        let Some(character) = source[cursor..].chars().next() else {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        };
        if !(character.is_alphanumeric() || character == '_') {
            self.show_error(self.message("cursor_not_word", &[]));
            return;
        }
        let start = source[..cursor]
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
            .last()
            .map_or(cursor, |(index, _)| index);
        let end = source[cursor..]
            .char_indices()
            .find(|(_, character)| !(character.is_alphanumeric() || *character == '_'))
            .map_or(source.len(), |(relative, _)| cursor + relative);
        self.ui.search_query = Some(source[start..end].to_owned());
        self.last_search_forward = forward;
        self.perform_search(forward);
    }

    fn apply_layout_action(&mut self, action: LayoutAction) {
        let closes_dirty = match action {
            LayoutAction::ClosePane => self
                .state
                .active_buffer_id()
                .and_then(|buffer| self.state.buffers.get(buffer))
                .is_some_and(|buffer| buffer.dirty),
            LayoutAction::CloseTab => self.state.active_tab_views().into_iter().any(|view| {
                self.state
                    .views
                    .get(view)
                    .and_then(|view| self.state.buffers.get(view.buffer))
                    .is_some_and(|buffer| buffer.dirty)
            }),
            _ => false,
        };
        if closes_dirty {
            self.show_error(self.message("dirty_close", &[]));
            return;
        }
        let operation = match action {
            LayoutAction::Split(axis) => EditorOperation::SplitPane {
                axis,
                ratio: self.configuration.settings.ui.panes.default_ratio,
            },
            LayoutAction::ClosePane => EditorOperation::ClosePane,
            LayoutAction::ResizePane(direction) => EditorOperation::ResizePane {
                delta: i16::from(direction)
                    * i16::try_from(self.configuration.settings.ui.panes.resize_step)
                        .unwrap_or(i16::MAX),
            },
            LayoutAction::FocusPane(offset) => EditorOperation::FocusPane {
                offset,
                wrap: self.configuration.settings.ui.panes.focus_wrap,
            },
            LayoutAction::NewTab => EditorOperation::NewTab,
            LayoutAction::CloseTab => EditorOperation::CloseTab,
            LayoutAction::FocusTab(offset) => EditorOperation::FocusTab {
                offset,
                wrap: self.configuration.settings.ui.tab_line.focus_wrap,
            },
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(operation)
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ui.dashboard = false;
            self.pending_keymap = None;
            self.input.cancel_pending();
        }
    }

    fn mouse_pane_geometry(&self) -> Option<(crate::editing::PaneId, u16, u16, u16, u16)> {
        let (global_column, global_row) = self.mouse_position?;
        let explorer_offset = self
            .ui
            .explorer
            .as_ref()
            .and_then(|_| {
                crate::terminal_ui::explorer_panel_width(
                    &self.configuration.settings,
                    self.terminal_width,
                )
            })
            .map_or(0, |width| width.saturating_add(1));
        if global_column < explorer_offset {
            return None;
        }
        let content_width = self.terminal_width.saturating_sub(explorer_offset);
        let tab = self.state.tabs.get(self.state.active_tab?)?;
        let tab_line_visible = self.configuration.settings.ui.tab_line.enabled
            && (!self.configuration.settings.ui.tab_line.hide_when_single
                || self.state.tabs.len() > 1);
        let top = u16::from(tab_line_visible);
        if global_row < top {
            return None;
        }
        let compact_layout;
        let pane_count = application_layout_pane_count(&tab.root);
        let layout_height = self
            .terminal_height
            .saturating_sub(top)
            .saturating_sub(u16::from(
                self.configuration.settings.ui.status_line.enabled,
            ));
        let layout = if content_width
            < self
                .configuration
                .settings
                .ui
                .panes
                .minimum_width
                .saturating_mul(u16::try_from(pane_count).unwrap_or(u16::MAX))
            || layout_height < self.configuration.settings.ui.panes.minimum_height
        {
            compact_layout = crate::editing::LayoutNode::Pane(tab.focused);
            &compact_layout
        } else {
            &tab.root
        };
        locate_mouse_pane(
            layout,
            explorer_offset,
            top,
            content_width,
            layout_height,
            global_column,
            global_row,
        )
    }

    fn focus_mouse_pane(&mut self) {
        let Some((pane, ..)) = self.mouse_pane_geometry() else {
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::FocusPaneId { pane })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    fn apply_mouse_cursor(&mut self, extend_selection: bool) {
        let Some((pane_id, column, row, pane_width, pane_height)) = self.mouse_pane_geometry()
        else {
            return;
        };
        let Some(view_id) = self.state.panes.get(pane_id).map(|pane| pane.view) else {
            return;
        };
        let view = &self.state.views[view_id];
        let original_cursor = view.cursor;
        let buffer_id = view.buffer;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let body_height = pane_height;
        if row >= body_height {
            return;
        }
        let digits = buffer.text.len_lines().max(1).to_string().len();
        let gutter = if self.configuration.settings.editor.line_numbers
            == crate::configuration::LineNumbers::Off
        {
            0
        } else {
            u16::try_from(
                digits
                    .saturating_add(
                        self.configuration
                            .settings
                            .editor
                            .line_number_separator
                            .len(),
                    )
                    .saturating_add(usize::from(
                        self.configuration.settings.editor.gutter_padding,
                    )),
            )
            .unwrap_or(u16::MAX)
        };
        let available = usize::from(pane_width.saturating_sub(gutter).max(1));
        let mut remaining_row = usize::from(row);
        let mut line_index = view.scroll_line;
        let mut wrapped_column = 0_usize;
        while line_index < buffer.text.len_lines() {
            let line = buffer.text.line(line_index).unwrap_or_default();
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let columns = configured_text_width(
                content,
                self.configuration.settings.editor.tab_width,
                self.configuration.settings.editor.ambiguous_width,
            );
            let rows = if self.configuration.settings.editor.wrap {
                columns.max(1).div_ceil(available)
            } else {
                1
            };
            if remaining_row < rows {
                wrapped_column = if self.configuration.settings.editor.wrap {
                    remaining_row.saturating_mul(available)
                } else {
                    0
                };
                break;
            }
            remaining_row = remaining_row.saturating_sub(rows);
            line_index = line_index.saturating_add(1);
        }
        if line_index >= buffer.text.len_lines() {
            return;
        }
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let content = line.strip_suffix('\n').unwrap_or(&line);
        let target_column =
            wrapped_column.saturating_add(usize::from(column.saturating_sub(gutter)));
        let mut display_column = 0_usize;
        let mut byte = 0_usize;
        for (relative, grapheme) in content.grapheme_indices(true) {
            let width = if grapheme == "\t" {
                self.configuration.settings.editor.tab_width
                    - (display_column % self.configuration.settings.editor.tab_width)
            } else {
                configured_grapheme_width(
                    grapheme,
                    self.configuration.settings.editor.ambiguous_width,
                )
            };
            if display_column.saturating_add(width) > target_column {
                byte = relative;
                break;
            }
            display_column = display_column.saturating_add(width);
            byte = relative.saturating_add(grapheme.len());
        }
        let position = ByteOffset(line_start.get().saturating_add(byte));
        let mut transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::FocusPaneId { pane: pane_id })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if extend_selection {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: Some(crate::editing::Selection {
                    anchor: self
                        .mouse_selection_anchor
                        .filter(|(anchor_view, _)| *anchor_view == view_id)
                        .map_or(original_cursor, |(_, anchor)| anchor),
                    head: position,
                    kind: crate::editing::SelectionKind::Character,
                }),
            });
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::VisualChar,
            });
        } else {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: None,
            });
        }
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else if !extend_selection {
            self.mouse_selection_anchor = Some((view_id, position));
        }
    }

    fn scroll_view(&mut self, kind: &str, count: u64) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let body = usize::from(
            self.terminal_height
                .saturating_sub(u16::from(
                    self.configuration.settings.ui.status_line.enabled,
                ))
                .max(1),
        );
        let percent = if kind.starts_with("half_") {
            self.configuration.settings.editor.half_page_percent
        } else {
            self.configuration.settings.editor.full_page_percent
        };
        let amount = body.saturating_mul(usize::from(percent)) / 100;
        let amount = amount
            .max(1)
            .saturating_mul(usize::try_from(count).unwrap_or(usize::MAX));
        let maximum = buffer.text.len_lines().saturating_sub(1);
        let first_line = if kind.ends_with("up") {
            view.scroll_line.saturating_sub(amount)
        } else {
            view.scroll_line.saturating_add(amount).min(maximum)
        };
        let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let target_line =
            current_line.clamp(first_line, first_line.saturating_add(body - 1).min(maximum));
        let old_start = buffer
            .text
            .line_to_byte(current_line)
            .unwrap_or(ByteOffset(0));
        let column = view.cursor.get().saturating_sub(old_start.get());
        let target_start = buffer
            .text
            .line_to_byte(target_line)
            .unwrap_or(ByteOffset(0));
        let target_text = buffer.text.line(target_line).unwrap_or_default();
        let target_content = target_text.strip_suffix('\n').unwrap_or(&target_text);
        let mut target = ByteOffset(target_start.get() + column.min(target_content.len()));
        while !buffer.text.is_char_boundary(target) {
            target = ByteOffset(target.get().saturating_sub(1));
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: target,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    fn start_open(&mut self, mut path: PathBuf) {
        if path.is_relative() {
            path = self.state.workspace_root.join(path);
        }
        match self.vfs.read(path) {
            Ok(id) => {
                self.pending_vfs.insert(id, PendingVfs::Open);
                self.ui.notification = Some(self.message("opening_file", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn start_reload(&mut self, force: bool) {
        let Some(buffer_id) = self.state.active_buffer_id() else {
            self.show_error(self.message("no_active_reload", &[]));
            return;
        };
        let buffer = &self.state.buffers[buffer_id];
        if buffer.dirty && !force {
            self.show_error(self.message("dirty_reload", &[]));
            return;
        }
        let Some(path) = buffer.path.clone() else {
            self.show_error(self.message("scratch_reload", &[]));
            return;
        };
        match self.vfs.read(path) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::Reload { buffer: buffer_id });
                self.ui.notification = Some(self.message("reloading", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn close_active_buffer(&mut self, force: bool) {
        let Some(view_id) = self.state.active_view else {
            self.ui.dashboard = true;
            return;
        };
        let buffer_id = self.state.views[view_id].buffer;
        if self.state.buffers[buffer_id].dirty && !force {
            self.show_error(self.message("dirty_close", &[]));
            return;
        }
        if let Err(error) = self.state.close_active_view() {
            self.show_error(error);
            return;
        }
        let still_viewed = self
            .state
            .views
            .iter()
            .any(|(_, view)| view.buffer == buffer_id);
        if !still_viewed {
            let documents = self
                .lsp_sessions
                .iter()
                .filter_map(|(server, state)| {
                    state
                        .documents
                        .get(&buffer_id)
                        .map(|document| (server.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer_id, documents);
            self.state.buffers.remove(buffer_id);
            self.lsp_synced_revisions.remove(&buffer_id);
        }
        if self.state.active_view.is_none()
            && let Some(buffer) = self.state.buffers.keys().next()
        {
            let _ = self.state.show_buffer(buffer);
        }
        self.ui.dashboard = self.state.active_view.is_none();
        if !self.ui.dashboard {
            self.sync_lsp_document();
        } else {
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            self.clear_lsp_ui_projection();
        }
        self.ui.notification = Some(if self.ui.dashboard {
            self.message("returned_dashboard", &[])
        } else {
            self.message("buffer_closed", &[])
        });
        self.ui.notification_error = false;
    }

    fn start_save(&mut self, quit_after: bool, requested_path: Option<PathBuf>, force: bool) {
        let Some(buffer_id) = self.state.active_buffer_id() else {
            self.show_error(self.message("no_active_save", &[]));
            return;
        };
        let buffer = &self.state.buffers[buffer_id];
        let mut path = match requested_path {
            Some(path) => path,
            None => {
                let Some(path) = buffer.path.clone() else {
                    self.show_error(self.message("scratch_no_name", &[]));
                    return;
                };
                path
            }
        };
        if path.is_relative() {
            path = self.state.workspace_root.join(path);
        }
        let revision = buffer.text.revision();
        let bytes = configured_file_bytes(&buffer.text, &self.configuration.settings.editor);
        let expectation = if force {
            WriteExpectation::Any
        } else if buffer.path.as_ref() == Some(&path) {
            WriteExpectation::Exact(buffer.persisted_bytes().to_vec())
        } else {
            WriteExpectation::Missing
        };
        match self
            .vfs
            .write_checked(path.clone(), bytes.clone(), expectation)
        {
            Ok(id) => {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::Save {
                        buffer: buffer_id,
                        revision,
                        bytes,
                        target: path,
                        quit_after,
                    },
                );
                self.ui.notification = Some(self.message("saving", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn process_configuration_result(&mut self) {
        let result = match self.configuration_worker.try_result() {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if !configuration_generation_is_newer(self.configuration_generation, result.generation) {
            return;
        }
        self.configuration_generation = result.generation;
        let automatic = result.automatic;
        let loaded = match result.candidate {
            Ok(loaded) => loaded,
            Err(diagnostic) => {
                self.retain_configuration_watch();
                let message = if automatic {
                    "auto_reload_rejected"
                } else {
                    "reload_rejected"
                };
                self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
                return;
            }
        };
        if let Err(diagnostic) = self.apply_configuration_candidate(loaded, automatic) {
            self.retain_configuration_watch();
            let message = if automatic {
                "auto_reload_rejected"
            } else {
                "reload_rejected"
            };
            self.show_error(self.message(message, &[("diagnostic", diagnostic)]));
        }
    }

    fn retain_configuration_watch(&self) {
        let _ = self.configuration_worker.configure_watch(
            self.configuration.settings.config_watch.clone(),
            self.configuration.settings.ui.theme.clone(),
        );
    }

    fn apply_configuration_candidate(
        &mut self,
        loaded: LoadedConfig,
        automatic: bool,
    ) -> Result<(), String> {
        let candidate = match resolve_configuration(&self.commands, loaded, true) {
            Ok((candidate, None)) => candidate,
            Ok((_, Some(diagnostic))) | Err(diagnostic) => return Err(diagnostic),
        };
        self.configuration_worker
            .configure_watch(
                candidate.settings.config_watch.clone(),
                candidate.settings.ui.theme.clone(),
            )
            .map_err(|error| error.to_string())?;
        let restart_required = candidate.settings.runtime.vfs_queue_capacity
            != self.configuration.settings.runtime.vfs_queue_capacity
            || candidate.settings.ui.alternate_screen
                != self.configuration.settings.ui.alternate_screen
            || candidate.settings.ui.bracketed_paste
                != self.configuration.settings.ui.bracketed_paste
            || candidate.settings.ui.focus_events != self.configuration.settings.ui.focus_events;
        self.cancel_pending_mapping();
        self.input
            .set_count_limit(candidate.settings.input.count_limit);
        self.ui.dashboard_actions = dashboard_actions(&candidate);
        self.state.policy = crate::editing::EditingPolicy {
            tab_width: candidate.settings.editor.tab_width,
            indent_width: candidate.settings.editor.indent_width,
            expand_tabs: candidate.settings.editor.expand_tabs,
            undo_break_on_text_commit: candidate.settings.editor.undo_break_on_text_commit,
            registers_enabled: candidate.settings.editor.registers.storage
                == crate::configuration::RegisterStorage::Internal,
            mirror_named_to_unnamed: candidate.settings.editor.registers.mirror_named_to_unnamed,
            ambiguous_width_wide: candidate.settings.editor.ambiguous_width
                == crate::configuration::AmbiguousWidth::Wide,
        };
        if candidate.settings.observability != self.configuration.settings.observability {
            self.logs = make_logs(&candidate.settings, true);
            self.logs.record(
                LogLevel::Info,
                "configuration",
                "logging-reconfigured",
                BTreeMap::new(),
            );
        }
        if candidate.settings.recovery != self.configuration.settings.recovery {
            self.recovery_discovery_queued = false;
            self.recovery_cleanup_queued = false;
            self.recovery_pruned_revisions.clear();
        }
        if !candidate.settings.recovery.enabled {
            self.recovery_scheduled.clear();
        }
        if !candidate.settings.history.command.enabled {
            self.command_history.clear();
        } else {
            let excess = self
                .command_history
                .len()
                .saturating_sub(candidate.settings.history.command.maximum_entries);
            self.command_history.drain(..excess);
        }
        if !candidate.settings.history.search.enabled {
            self.search_history.clear();
        } else {
            let excess = self
                .search_history
                .len()
                .saturating_sub(candidate.settings.history.search.maximum_entries);
            self.search_history.drain(..excess);
        }
        self.diagnostics.set_limits(
            candidate.settings.workspace.maximum_diagnostics,
            candidate
                .settings
                .workspace
                .maximum_diagnostics_per_owner_uri,
        );
        self.sync_ui_diagnostics();
        if self.ui.dashboard && !candidate.settings.startup.show_dashboard {
            self.state.open_text(TextBuffer::from_text(""), None);
            self.ui.dashboard = false;
        }
        let plugins_changed = candidate.settings.plugins != self.configuration.settings.plugins;
        if plugins_changed {
            for (plugin, (load, token)) in self.package_prepared.drain().collect::<Vec<_>>() {
                let _ = self.plugins.unload_generation(&plugin, load);
                let _ = self.packages.request(PackageOperation::Abort { token });
            }
            self.package_startup_pending.clear();
            self.package_managed_plugins.clear();
            let ids = self.plugins.statuses.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                let _ = self.plugins.disable(&id);
                self.commands.unregister_plugin(&id);
            }
        }
        self.configuration = candidate;
        if plugins_changed {
            self.load_configured_plugins();
            self.package_startup_rollback_attempted = false;
            match self.packages.request(PackageOperation::Activations) {
                Ok(id) => self.package_startup_request = Some(id),
                Err(error) => self.show_error(error),
            }
        }
        self.rebuild_matcher_cache();
        if let Some(menu) = &self.ui.tool_menu {
            let category = menu.category.clone();
            self.rebuild_tool_menu(&category, None);
        }
        self.ui.config_diagnostic = None;
        self.ui.notification = Some(if restart_required {
            self.message("reloaded_restart", &[])
        } else if automatic {
            self.message("auto_reloaded", &[])
        } else {
            self.message("reloaded", &[])
        });
        self.ui.notification_error = false;
        Ok(())
    }

    fn process_vfs_results(&mut self) {
        loop {
            let result = match self.vfs.try_result() {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(error) => {
                    self.show_error(error);
                    break;
                }
            };
            match result {
                VfsResult::RecoveryDiscovered { id, discovery } => {
                    if matches!(
                        self.pending_vfs.remove(&id),
                        Some(PendingVfs::RecoveryDiscovery)
                    ) {
                        self.recovery_records = discovery.records;
                        if !discovery.diagnostics.is_empty() {
                            self.show_error(discovery.diagnostics.join("; "));
                        }
                    }
                }
                VfsResult::RecoveryCleaned { id, cleanup } => {
                    if matches!(
                        self.pending_vfs.remove(&id),
                        Some(PendingVfs::RecoveryCleanup)
                    ) {
                        self.recovery_records
                            .retain(|(path, _)| !cleanup.deleted.contains(path));
                        let pruned_live = self
                            .recovery_revisions
                            .iter()
                            .filter_map(|(buffer, revision)| {
                                let path = self.recovery_path(*buffer);
                                let newer_write_pending =
                                    self.pending_vfs.values().any(|pending| {
                                        matches!(
                                            pending,
                                            PendingVfs::RecoveryWrite {
                                                buffer: pending_buffer,
                                                ..
                                            } if pending_buffer == buffer
                                        )
                                    });
                                (cleanup.deleted.contains(&path) && !newer_write_pending)
                                    .then_some((*buffer, *revision))
                            })
                            .collect::<Vec<_>>();
                        for (buffer, revision) in pruned_live {
                            self.recovery_revisions.remove(&buffer);
                            self.recovery_pruned_revisions.insert(buffer, revision);
                        }
                        if !cleanup.diagnostics.is_empty() {
                            self.show_error(cleanup.diagnostics.join("; "));
                        } else if !cleanup.deleted.is_empty() {
                            self.ui.notification = Some(self.message(
                                "recovery_pruned",
                                &[("count", cleanup.deleted.len().to_string())],
                            ));
                            self.ui.notification_error = false;
                        }
                    }
                }
                VfsResult::RecoveryWritten {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::RecoveryWrite { .. }), Ok(())) => {
                            self.recovery_cleanup_queued = false;
                        }
                        (Some(PendingVfs::RecoveryWrite { buffer, revision }), Err(error)) => {
                            if self.recovery_revisions.get(&buffer) == Some(&revision) {
                                self.recovery_revisions.remove(&buffer);
                            }
                            self.show_error(error);
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Deleted {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::CrashDelete), Ok(())) => {
                            self.ui.notification = Some(self.message("crash_cleared", &[]));
                            self.ui.notification_error = false;
                        }
                        (
                            Some(PendingVfs::RecoveryDelete {
                                path: expected,
                                buffer,
                            }),
                            Ok(()),
                        ) => {
                            self.recovery_records
                                .retain(|(record_path, _)| *record_path != expected);
                            if let Some((buffer, expected_revision)) = buffer {
                                if self.recovery_revisions.get(&buffer) == Some(&expected_revision)
                                {
                                    self.recovery_revisions.remove(&buffer);
                                }
                                if self.recovery_pruned_revisions.get(&buffer)
                                    == Some(&expected_revision)
                                {
                                    self.recovery_pruned_revisions.remove(&buffer);
                                }
                            } else {
                                self.ui.notification =
                                    Some(self.message("recovery_discarded", &[]));
                                self.ui.notification_error = false;
                            }
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::ResourceResolved { id, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::WorkspaceResourceResolve { proposal }), Ok(intent)) => {
                            let mut finish = false;
                            if let Some(build) = &mut self.pending_workspace_proposal
                                && build.id == proposal
                            {
                                if let ResourceIntentKind::Rename { target, .. } = &intent.kind
                                    && self
                                        .state
                                        .buffers
                                        .iter()
                                        .any(|(_, buffer)| buffer.path.as_ref() == Some(target))
                                {
                                    if let Some(build) = self.pending_workspace_proposal.take() {
                                        self.reject_pending_workspace_proposal(
                                            build,
                                            "rename target already has an editor buffer".into(),
                                        )
                                    }
                                    continue;
                                }
                                build.resource_intents.push(intent);
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            if finish {
                                self.finish_workspace_proposal();
                            }
                        }
                        (Some(PendingVfs::WorkspaceResourceResolve { .. }), Err(error)) => {
                            if let Some(build) = self.pending_workspace_proposal.take() {
                                self.reject_pending_workspace_proposal(build, error.to_string())
                            }
                        }
                        _ => {}
                    }
                }
                VfsResult::ResourceApplied { id, intent, result } => {
                    self.resource_apply_pending.remove(&id);
                    match result {
                        Ok(()) => match intent.kind {
                            ResourceIntentKind::Rename { target, .. } => {
                                if let Some((buffer, _)) =
                                    self.state.buffers.iter().find(|(_, buffer)| {
                                        buffer.path.as_ref() == Some(&intent.source)
                                    })
                                {
                                    self.state.buffers[buffer].path = Some(target.clone());
                                    self.record_recent_file(target);
                                }
                            }
                            ResourceIntentKind::Delete => {
                                if let Some((buffer, _)) =
                                    self.state.buffers.iter().find(|(_, buffer)| {
                                        buffer.path.as_ref() == Some(&intent.source)
                                    })
                                {
                                    self.state.buffers[buffer].path = None;
                                    self.state.buffers[buffer].dirty = true;
                                }
                            }
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::CreateTarget { id, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::WorkspaceProposalCreate {
                                proposal,
                                requested_path,
                            }),
                            Ok(path),
                        ) => {
                            let mut finish = false;
                            if let Some(build) = &mut self.pending_workspace_proposal
                                && build.id == proposal
                            {
                                build.resolved_paths.insert(requested_path, path.clone());
                                build.creates.push(path.clone());
                                build.snapshots.push(FileSnapshot {
                                    canonical_path: path,
                                    revision: None,
                                    bytes: Vec::new(),
                                });
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            if finish {
                                self.finish_workspace_proposal();
                            }
                        }
                        (Some(PendingVfs::WorkspaceProposalCreate { .. }), Err(error)) => {
                            if let Some(build) = self.pending_workspace_proposal.take() {
                                self.reject_pending_workspace_proposal(build, error.to_string())
                            }
                        }
                        _ => {}
                    }
                }
                VfsResult::Read { id, path, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    let startup_primary = match &pending {
                        Some(PendingVfs::StartupOpen { primary }) => Some(*primary),
                        _ => None,
                    };
                    if let Some(PendingVfs::ExplorerPreview {
                        path: requested,
                        generation,
                    }) = &pending
                    {
                        if *generation == self.explorer_preview_generation
                            && self.explorer_preview_path.as_ref() == Some(requested)
                        {
                            self.explorer_preview_lines = match result {
                                Ok(bytes) => {
                                    let maximum = self
                                        .configuration
                                        .settings
                                        .ui
                                        .explorer
                                        .maximum_preview_bytes;
                                    let bytes = &bytes[..bytes.len().min(maximum)];
                                    match std::str::from_utf8(bytes) {
                                        Ok(text) => text
                                            .lines()
                                            .take(
                                                self.configuration
                                                    .settings
                                                    .ui
                                                    .explorer
                                                    .maximum_preview_lines,
                                            )
                                            .map(|line| {
                                                line.chars()
                                                    .filter(|character| {
                                                        !character.is_control()
                                                            || *character == '\t'
                                                    })
                                                    .take(4096)
                                                    .collect()
                                            })
                                            .collect(),
                                        Err(_) => {
                                            vec![self.message("explorer_preview_binary", &[])]
                                        }
                                    }
                                }
                                Err(error) => vec![self.message(
                                    "explorer_preview_failed",
                                    &[("diagnostic", error.to_string())],
                                )],
                            };
                            self.sync_explorer_ui();
                        }
                        continue;
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(PendingVfs::WorkspaceProposalRead {
                                proposal,
                                requested_path,
                            }) => {
                                let mut finish = false;
                                if let Some(build) = &mut self.pending_workspace_proposal
                                    && build.id == proposal
                                {
                                    build.resolved_paths.insert(requested_path, path.clone());
                                    build.snapshots.push(FileSnapshot {
                                        canonical_path: path,
                                        revision: None,
                                        bytes: text.to_bytes(),
                                    });
                                    build.waiting = build.waiting.saturating_sub(1);
                                    finish = build.waiting == 0;
                                }
                                if finish {
                                    self.finish_workspace_proposal();
                                }
                            }
                            Some(PendingVfs::SessionRestore { requested_path }) => {
                                self.accept_session_restore_read(requested_path, path, text)
                            }
                            Some(PendingVfs::Open) => {
                                self.state.open_text(text, Some(path.clone()));
                                self.record_recent_file(path.clone());
                                self.ui.dashboard = false;
                                self.ui.notification = Some(
                                    self.message("opened", &[("path", path.display().to_string())]),
                                );
                                self.ui.notification_error = false;
                            }
                            Some(PendingVfs::LinterRead { generation, raw }) => {
                                let uri = file_uri(&path);
                                let mut diagnostics = Vec::new();
                                for raw in raw {
                                    let start = text
                                        .line_to_byte(raw.line)
                                        .ok()
                                        .map(|offset| {
                                            ByteOffset(
                                                (offset.get() + raw.column).min(text.len_bytes()),
                                            )
                                        })
                                        .filter(|offset| text.validate_offset(*offset).is_ok())
                                        .unwrap_or(ByteOffset(0));
                                    diagnostics.push(Diagnostic {
                                        uri: uri.clone(),
                                        version: None,
                                        range: ByteRange::new(start, start).expect("empty"),
                                        severity: raw.severity,
                                        source: "linter".into(),
                                        code: None,
                                        message: raw.message,
                                        related: Vec::new(),
                                        tags: Vec::new(),
                                        fixes: Vec::new(),
                                        timestamp: crate::clock::system_time(),
                                    });
                                }
                                let mut finish = false;
                                if let Some(build) = self.pending_linter_builds.get_mut(&generation)
                                {
                                    build.grouped.entry(uri).or_default().extend(diagnostics);
                                    build.waiting = build.waiting.saturating_sub(1);
                                    finish = build.waiting == 0;
                                }
                                if finish {
                                    self.finish_linter_build(generation);
                                }
                            }
                            Some(PendingVfs::MatcherNavigation { source }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let start = self.state.buffers[buffer]
                                    .text
                                    .line_to_byte(source.line)
                                    .ok()
                                    .map(|offset| {
                                        ByteOffset(
                                            (offset.get() + source.column)
                                                .min(self.state.buffers[buffer].text.len_bytes()),
                                        )
                                    })
                                    .filter(|offset| {
                                        self.state.buffers[buffer]
                                            .text
                                            .validate_offset(*offset)
                                            .is_ok()
                                    })
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.record_recent_file(path);
                                self.ui.dashboard = false;
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::PersistedNavigation { location }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let cursor = ByteOffset(location.byte.min(length));
                                let anchor = ByteOffset(location.anchor.min(length));
                                self.state.views[view].cursor = cursor;
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].selection =
                                    (anchor != cursor).then_some(crate::editing::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editing::SelectionKind::Character,
                                    });
                                self.record_recent_file(path);
                                self.ui.dashboard = false;
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::LspNavigation { range, encoding }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let position = self.state.buffers.get(buffer).and_then(|buffer| {
                                    PositionCodec::new(&buffer.text)
                                        .lsp_to_byte(
                                            range.start.line,
                                            range.start.character,
                                            encoding,
                                        )
                                        .ok()
                                });
                                if let Some(position) = position {
                                    let transaction = EditorTransaction::new(self.transaction_id())
                                        .with_precondition(Precondition::ViewDisplays {
                                            view,
                                            buffer,
                                        })
                                        .with_operation(EditorOperation::SetCursor {
                                            view,
                                            position,
                                        })
                                        .with_undo_policy(UndoPolicy::Ignore);
                                    if let Err(error) = self.state.apply_transaction(&transaction) {
                                        self.show_error(error);
                                    }
                                } else {
                                    self.show_error(self.message("lsp_invalid_position", &[]));
                                }
                                self.ui.dashboard = false;
                            }
                            Some(PendingVfs::WorkspaceSearchNavigation { range }) => {
                                let start = ByteOffset(range.start);
                                let end = ByteOffset(range.end);
                                if text.validate_offset(start).is_err()
                                    || text.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = self.state.open_text(text, Some(path));
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: start,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                }
                                self.ui.dashboard = false;
                            }
                            Some(PendingVfs::ExplorerOpen { split }) => {
                                if split {
                                    self.apply_layout_action(LayoutAction::Split(
                                        crate::editing::Axis::Vertical,
                                    ));
                                }
                                self.state.open_text(text, Some(path));
                                self.ui.dashboard = false;
                            }
                            Some(PendingVfs::StartupOpen { primary }) => {
                                let (buffer, _) = self.state.open_text(text, Some(path.clone()));
                                if primary {
                                    self.startup_primary_buffer = Some(buffer);
                                }
                                self.ui.dashboard = false;
                                self.ui.notification = Some(
                                    self.message("opened", &[("path", path.display().to_string())]),
                                );
                                self.ui.notification_error = false;
                            }
                            Some(PendingVfs::Reload { buffer }) => {
                                if self
                                    .state
                                    .replace_buffer_from_disk(buffer, text, path.clone())
                                {
                                    self.ui.notification = Some(self.message(
                                        "reloaded_file",
                                        &[("path", path.display().to_string())],
                                    ));
                                    self.ui.notification_error = false;
                                } else {
                                    self.show_error(self.message("reload_closed", &[]));
                                }
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => {
                            if matches!(pending, Some(PendingVfs::WorkspaceProposalRead { .. })) {
                                if let Some(build) = self.pending_workspace_proposal.take() {
                                    self.reject_pending_workspace_proposal(
                                        build,
                                        error.to_string(),
                                    );
                                }
                            } else {
                                self.show_error(error);
                            }
                        }
                    }
                    if startup_primary.is_some() {
                        self.startup_reads_pending = self.startup_reads_pending.saturating_sub(1);
                        self.finish_startup_reads_if_complete();
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::Completion {
                                generation,
                                prompt_input,
                                fragment,
                            }),
                            Ok(paths),
                        ) if generation == self.completion_generation
                            && self
                                .ui
                                .prompt
                                .as_ref()
                                .is_some_and(|prompt| prompt.input == prompt_input) =>
                        {
                            let prefix = PathBuf::from(&fragment)
                                .file_name()
                                .map_or_else(String::new, |name| {
                                    name.to_string_lossy().to_lowercase()
                                });
                            let limit = self.configuration.settings.runtime.completion_limit;
                            let detail =
                                self.configuration.settings.command_labels["file.open"].clone();
                            let workspace = self.state.workspace_root.clone();
                            let select_first =
                                self.configuration.settings.input.completion_select_first;
                            if let Some(prompt) = &mut self.ui.prompt {
                                prompt.completions = paths
                                    .into_iter()
                                    .filter(|path| {
                                        path.file_name().is_some_and(|name| {
                                            name.to_string_lossy()
                                                .to_lowercase()
                                                .starts_with(&prefix)
                                        })
                                    })
                                    .take(limit)
                                    .map(|path| CompletionItem {
                                        label: path
                                            .strip_prefix(&workspace)
                                            .unwrap_or(&path)
                                            .display()
                                            .to_string(),
                                        detail: detail.clone(),
                                    })
                                    .collect();
                                prompt.selected_completion =
                                    (select_first && !prompt.completions.is_empty()).then_some(0);
                            }
                        }
                        (Some(PendingVfs::Completion { .. }), Err(_)) => {}
                        (Some(PendingVfs::Completion { .. }), Ok(_)) => {}
                        (Some(PendingVfs::LinterRead { generation, .. }), Err(error)) => {
                            let mut finish = false;
                            if let Some(build) = self.pending_linter_builds.get_mut(&generation) {
                                build.failed = true;
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            self.logs.record(
                                LogLevel::Error,
                                "linter",
                                "source-read-failed",
                                BTreeMap::from([("error".into(), error.to_string())]),
                            );
                            if finish {
                                self.finish_linter_build(generation);
                            }
                        }
                        (Some(PendingVfs::SessionRestore { .. }), Err(_)) => {
                            self.fail_session_restore()
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { id, path, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::Save {
                                buffer,
                                revision,
                                bytes,
                                target,
                                quit_after,
                            }),
                            Ok(()),
                        ) => {
                            if let Some(buffer_state) = self.state.buffers.get_mut(buffer) {
                                buffer_state.mark_saved_as(revision, bytes, Some(target));
                                let still_dirty = buffer_state.dirty;
                                let lsp_document = buffer_state.path.clone().and_then(|path| {
                                    Some(LspDocument {
                                        language_id: language_id_for_path(&path)?.into(),
                                        path,
                                        version: buffer_state.text.revision().0,
                                        text: buffer_state.text.to_string(),
                                    })
                                });
                                self.ui.notification = Some(if still_dirty {
                                    self.message(
                                        "saved_dirty",
                                        &[
                                            ("revision", revision.0.to_string()),
                                            ("path", path.display().to_string()),
                                        ],
                                    )
                                } else {
                                    self.message("saved", &[("path", path.display().to_string())])
                                });
                                self.ui.notification_error = false;
                                if let Some(server) = self.active_lsp_server.as_deref()
                                    && let Some(document) = lsp_document
                                {
                                    let _ = self.lsp.save(server, document);
                                }
                                if quit_after && !still_dirty {
                                    if self
                                        .state
                                        .buffers
                                        .iter()
                                        .any(|(_, candidate)| candidate.dirty)
                                    {
                                        self.show_error(self.message("saved_others_dirty", &[]));
                                    } else {
                                        self.should_quit = true;
                                    }
                                }
                            }
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        if self.ui.prompt.is_none() {
            if let Some(view) = self.state.active_view {
                let mode = self.state.views[view].mode;
                self.previous_mode = Some((view, mode));
                self.set_mode(match kind {
                    PromptKind::Command => EditorMode::CommandLine,
                    PromptKind::SearchForward
                    | PromptKind::SearchBackward
                    | PromptKind::WorkspaceTrust => EditorMode::Prompt,
                });
            }
        }
        let mut prompt = PromptState::new(kind);
        prompt.cursor = initial.len();
        prompt.input = initial;
        self.ui.prompt = Some(prompt);
        self.history_index = None;
        self.refresh_completions();
    }

    fn close_prompt(&mut self) {
        self.ui.prompt = None;
        self.history_index = None;
        if let Some((view, mode)) = self.previous_mode.take() {
            if self.state.views.contains_key(view) {
                self.set_specific_mode(view, mode);
            }
        }
    }

    fn prompt_insert(&mut self, text: &str) {
        if let Some(prompt) = &mut self.ui.prompt {
            if prompt.input.is_char_boundary(prompt.cursor) {
                prompt.input.insert_str(prompt.cursor, text);
                prompt.cursor += text.len();
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
        self.refresh_completions();
    }

    fn prompt_backspace(&mut self) {
        if let Some(prompt) = &mut self.ui.prompt {
            let before = &prompt.input[..prompt.cursor];
            let previous = before
                .grapheme_indices(true)
                .next_back()
                .map(|(index, _)| index);
            if let Some(previous) = previous {
                prompt.input.replace_range(previous..prompt.cursor, "");
                prompt.cursor = previous;
                prompt.error = None;
                prompt.selected_completion = None;
            }
        }
        self.refresh_completions();
    }

    fn prompt_move(&mut self, movement: PromptMove) {
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.cursor = match movement {
                PromptMove::Home => 0,
                PromptMove::End => prompt.input.len(),
                PromptMove::Left => prompt.input[..prompt.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(index, _)| index),
                PromptMove::Right => prompt.input[prompt.cursor..]
                    .grapheme_indices(true)
                    .nth(1)
                    .map_or(prompt.input.len(), |(index, _)| prompt.cursor + index),
            };
        }
    }

    fn prompt_history(&mut self, direction: i8) {
        let Some(kind) = self.ui.prompt.as_ref().map(|prompt| prompt.kind) else {
            return;
        };
        if kind == PromptKind::WorkspaceTrust {
            return;
        }
        let (enabled, history) = if kind == PromptKind::Command {
            (
                self.configuration.settings.history.command.enabled,
                &self.command_history,
            )
        } else {
            (
                self.configuration.settings.history.search.enabled,
                &self.search_history,
            )
        };
        if !enabled {
            return;
        }
        if history.is_empty() {
            return;
        }
        let current = self.history_index.unwrap_or(history.len());
        let next = if direction < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(history.len())
        };
        self.history_index = (next < history.len()).then_some(next);
        let selected = self
            .history_index
            .and_then(|index| history.get(index).cloned())
            .unwrap_or_default();
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.input = selected;
            prompt.cursor = prompt.input.len();
            prompt.error = None;
            prompt.selected_completion = None;
        }
        self.refresh_completions();
    }

    fn prompt_complete(&mut self, direction: i8) {
        let Some(prompt) = &mut self.ui.prompt else {
            return;
        };
        if prompt.completions.is_empty() {
            return;
        }
        let length = prompt.completions.len();
        let wrap = self.configuration.settings.input.completion_wrap;
        prompt.selected_completion = Some(match (prompt.selected_completion, direction < 0) {
            (None, false) => 0,
            (None, true) => length - 1,
            (Some(current), false) if wrap => (current + 1) % length,
            (Some(current), true) if wrap => current.checked_sub(1).unwrap_or(length - 1),
            (Some(current), false) => (current + 1).min(length - 1),
            (Some(current), true) => current.saturating_sub(1),
        });
    }

    fn accept_completion(&mut self) -> bool {
        let Some(prompt) = &mut self.ui.prompt else {
            return false;
        };
        let Some(index) = prompt.selected_completion else {
            return false;
        };
        let Some(item) = prompt.completions.get(index) else {
            return false;
        };
        let label = item.label.clone();
        if let Some((command, _)) = prompt.input.split_once(char::is_whitespace) {
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| matches!(id.as_str(), "file.open" | "workspace.root.add"))
            {
                prompt.input = format!("{command} {label}");
            } else {
                prompt.input = label;
            }
        } else {
            prompt.input = label;
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(prompt.input.as_str())
                .is_some_and(|id| {
                    matches!(
                        id.as_str(),
                        "file.open" | "file.save" | "workspace.root.add"
                    )
                })
            {
                prompt.input.push(' ');
            }
        }
        prompt.cursor = prompt.input.len();
        prompt.selected_completion = None;
        prompt.error = None;
        self.refresh_completions();
        true
    }

    fn refresh_completions(&mut self) {
        let Some(prompt) = &self.ui.prompt else {
            return;
        };
        if prompt.kind != PromptKind::Command {
            if let Some(prompt) = &mut self.ui.prompt {
                prompt.completions.clear();
                prompt.selected_completion = None;
            }
            return;
        }
        let input = prompt.input.clone();
        let mut candidates = BTreeMap::<String, String>::new();
        if let Some((command, fragment)) = input.split_once(char::is_whitespace) {
            if self
                .configuration
                .settings
                .input
                .command_aliases
                .get(command)
                .is_some_and(|id| {
                    matches!(
                        id.as_str(),
                        "file.open" | "file.save" | "workspace.root.add"
                    )
                })
            {
                let fragment = fragment.trim();
                let query = fragment.to_lowercase();
                for (_, buffer) in &self.state.buffers {
                    if let Some(path) = &buffer.path {
                        let label = path.display().to_string();
                        if label.to_lowercase().contains(&query) {
                            let detail = &self.configuration.settings.command_labels["file.open"];
                            candidates.insert(label, detail.clone());
                        }
                    }
                }
                self.completion_generation = self.completion_generation.saturating_add(1);
                let generation = self.completion_generation;
                let entered = PathBuf::from(fragment);
                let parent = entered.parent().filter(|path| !path.as_os_str().is_empty());
                let directory = parent.map_or_else(
                    || self.state.workspace_root.clone(),
                    |parent| {
                        if parent.is_absolute() {
                            parent.to_path_buf()
                        } else {
                            self.state.workspace_root.join(parent)
                        }
                    },
                );
                if let Ok(id) = self.vfs.list_directory(
                    directory,
                    self.configuration.settings.runtime.completion_limit,
                ) {
                    self.pending_vfs.insert(
                        id,
                        PendingVfs::Completion {
                            generation,
                            prompt_input: input.clone(),
                            fragment: fragment.into(),
                        },
                    );
                }
            }
        } else {
            let query = input.to_lowercase();
            for (alias, command_id) in &self.configuration.settings.input.command_aliases {
                let detail = self
                    .configuration
                    .settings
                    .command_labels
                    .get(command_id)
                    .expect("resolved configuration validates command labels");
                if alias.starts_with(&query) || detail.to_lowercase().contains(&query) {
                    candidates.insert(alias.clone(), detail.into());
                }
            }
            for spec in self.commands.specs() {
                let id = spec.id.as_str();
                let label = &self.configuration.settings.command_labels[id];
                if id.starts_with(&query) || label.to_lowercase().contains(&query) {
                    candidates.insert(id.into(), label.clone());
                }
            }
        }
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.completions = candidates
                .into_iter()
                .take(self.configuration.settings.runtime.completion_limit)
                .map(|(label, detail)| CompletionItem { label, detail })
                .collect();
            if prompt
                .selected_completion
                .is_some_and(|index| index >= prompt.completions.len())
            {
                prompt.selected_completion = None;
            }
            if prompt.selected_completion.is_none()
                && self.configuration.settings.input.completion_select_first
                && !prompt.completions.is_empty()
            {
                prompt.selected_completion = Some(0);
            }
        }
    }

    fn record_prompt_history(&mut self, kind: PromptKind, input: &str) {
        if input.is_empty() {
            return;
        }
        if kind == PromptKind::WorkspaceTrust {
            return;
        }
        let configured = if kind == PromptKind::Command {
            &self.configuration.settings.history.command
        } else {
            &self.configuration.settings.history.search
        };
        if !configured.enabled || configured.maximum_entries == 0 {
            return;
        }
        let history = if kind == PromptKind::Command {
            &mut self.command_history
        } else {
            &mut self.search_history
        };
        if history.last().is_some_and(|entry| entry == input) {
            return;
        }
        history.push(input.into());
        let excess = history.len().saturating_sub(configured.maximum_entries);
        if excess > 0 {
            history.drain(..excess);
        }
    }

    fn submit_prompt(&mut self) {
        if self
            .configuration
            .settings
            .input
            .completion_accept_on_submit
            && self.accept_completion()
        {
            return;
        }
        let Some(prompt) = self.ui.prompt.clone() else {
            return;
        };
        if prompt.kind != PromptKind::WorkspaceTrust {
            self.record_prompt_history(prompt.kind, &prompt.input);
        }
        match prompt.kind {
            PromptKind::Command => match parse_ex(
                &prompt.input,
                &self.configuration.settings.input.command_aliases,
            ) {
                Ok(invocation) => {
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(error.to_string());
                    }
                }
            },
            PromptKind::WorkspaceTrust => match CommandInvocation::new(
                "workspace.trust.confirm",
                InvocationSource::CommandLine,
            ) {
                Ok(invocation) => {
                    let invocation =
                        invocation.with_argument("phrase", CommandValue::String(prompt.input));
                    self.close_prompt();
                    self.invoke(invocation);
                }
                Err(error) => {
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(error.to_string());
                    }
                }
            },
            PromptKind::SearchForward | PromptKind::SearchBackward => {
                if prompt.input.is_empty() {
                    let message = self.message("search_empty", &[]);
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(message);
                    }
                    return;
                }
                if self.state.active_view.is_none() {
                    let message = self.message("search_no_buffer", &[]);
                    if let Some(active) = &mut self.ui.prompt {
                        active.error = Some(message);
                    }
                    return;
                }
                if self.configuration.settings.search.regex {
                    let source = self
                        .state
                        .active_buffer_id()
                        .and_then(|buffer| self.state.buffers.get(buffer))
                        .map(|buffer| buffer.text.to_string())
                        .unwrap_or_default();
                    if let Err(error) = crate::input::search_matches(
                        &source,
                        &prompt.input,
                        self.configuration.settings.search.smart_case,
                        true,
                    ) {
                        if let Some(active) = &mut self.ui.prompt {
                            active.error = Some(error.to_string());
                        }
                        return;
                    }
                }
                let forward = prompt.kind == PromptKind::SearchForward;
                self.ui.search_query = Some(prompt.input);
                self.last_search_forward = forward;
                self.close_prompt();
                self.perform_search(forward);
            }
        }
    }

    fn perform_search(&mut self, forward: bool) {
        let Some(query) = self.ui.search_query.clone() else {
            self.show_error(self.message("no_previous_search", &[]));
            return;
        };
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor_offset = view.cursor;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let source = buffer.text.to_string();
        let matches = match crate::input::search_matches(
            &source,
            &query,
            self.configuration.settings.search.smart_case,
            self.configuration.settings.search.regex,
        ) {
            Ok(matches) => matches,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let wrap = self.configuration.settings.search.wrap;
        let cursor = cursor_offset.get().min(source.len());
        let target = if forward {
            let start = if cursor < source.len() {
                crate::document::next_grapheme_boundary(&buffer.text, cursor_offset).get()
            } else {
                cursor
            };
            matches
                .iter()
                .find(|matched| matched.start >= start)
                .or_else(|| wrap.then(|| matches.first()).flatten())
                .map(|matched| matched.start)
        } else {
            matches
                .iter()
                .rev()
                .find(|matched| matched.start < cursor)
                .or_else(|| wrap.then(|| matches.last()).flatten())
                .map(|matched| matched.start)
        };
        let Some(target) = target else {
            self.show_error(self.message("pattern_not_found", &[("query", query)]));
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(target),
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }

    fn set_mode(&mut self, mode: EditorMode) {
        if let Some(view) = self.state.active_view {
            self.set_specific_mode(view, mode);
        }
    }

    fn set_specific_mode(&mut self, view: ViewId, mode: EditorMode) {
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewExists(view))
            .with_operation(EditorOperation::SetMode { view, mode })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer = &self.state.buffers[view.buffer];
        let line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
        let body_height = usize::from(
            self.terminal_height
                .saturating_sub(u16::from(
                    self.configuration.settings.ui.status_line.enabled,
                ))
                .max(1),
        );
        let first = if line < view.scroll_line {
            line
        } else if line >= view.scroll_line.saturating_add(body_height) {
            line.saturating_sub(body_height - 1)
        } else {
            return;
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetScroll {
                view: view_id,
                first_line: first,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        }
    }

    fn transaction_id(&mut self) -> TransactionId {
        let id = TransactionId(self.next_transaction);
        self.next_transaction = self.next_transaction.saturating_add(1);
        id
    }

    fn message(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut message = self
            .configuration
            .settings
            .messages
            .get(key)
            .expect("resolved configuration validates required messages")
            .clone();
        for (name, value) in replacements {
            message = message.replace(&format!("{{{name}}}"), value);
        }
        message
    }

    fn show_error(&mut self, error: impl std::fmt::Display) {
        self.ui.notification = Some(error.to_string());
        self.ui.notification_error = true;
    }
}

const fn configuration_generation_is_newer(current: u64, candidate: u64) -> bool {
    candidate > current
}

fn application_layout_pane_count(node: &crate::editing::LayoutNode) -> usize {
    match node {
        crate::editing::LayoutNode::Pane(_) => 1,
        crate::editing::LayoutNode::Split { first, second, .. } => {
            application_layout_pane_count(first)
                .saturating_add(application_layout_pane_count(second))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn locate_mouse_pane(
    node: &crate::editing::LayoutNode,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    column: u16,
    row: u16,
) -> Option<(crate::editing::PaneId, u16, u16, u16, u16)> {
    match node {
        crate::editing::LayoutNode::Pane(pane)
            if column >= x
                && column < x.saturating_add(width)
                && row >= y
                && row < y.saturating_add(height) =>
        {
            Some((
                *pane,
                column.saturating_sub(x),
                row.saturating_sub(y),
                width,
                height,
            ))
        }
        crate::editing::LayoutNode::Pane(_) => None,
        crate::editing::LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            crate::editing::Axis::Horizontal => {
                let available = width.saturating_sub(1);
                let first_width = available.saturating_mul(*ratio) / 1000;
                locate_mouse_pane(first, x, y, first_width, height, column, row).or_else(|| {
                    locate_mouse_pane(
                        second,
                        x.saturating_add(first_width).saturating_add(1),
                        y,
                        available.saturating_sub(first_width),
                        height,
                        column,
                        row,
                    )
                })
            }
            crate::editing::Axis::Vertical => {
                let available = height.saturating_sub(1);
                let first_height = available.saturating_mul(*ratio) / 1000;
                locate_mouse_pane(first, x, y, width, first_height, column, row).or_else(|| {
                    locate_mouse_pane(
                        second,
                        x,
                        y.saturating_add(first_height).saturating_add(1),
                        width,
                        available.saturating_sub(first_height),
                        column,
                        row,
                    )
                })
            }
        },
    }
}

fn configured_grapheme_width(
    grapheme: &str,
    ambiguous_width: crate::configuration::AmbiguousWidth,
) -> usize {
    let width = if ambiguous_width == crate::configuration::AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(grapheme)
    } else {
        UnicodeWidthStr::width(grapheme)
    };
    width.max(1)
}

fn configured_text_width(
    text: &str,
    tab_width: usize,
    ambiguous_width: crate::configuration::AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            column += tab_width - (column % tab_width);
        } else {
            column += configured_grapheme_width(grapheme, ambiguous_width);
        }
    }
    column
}

fn configured_file_bytes(
    text: &TextBuffer,
    configured: &crate::configuration::EditorConfig,
) -> Vec<u8> {
    use crate::configuration::{BomPolicy, FinalNewlinePolicy, LineEndingPolicy};
    let mut source = text.to_string();
    match configured.final_newline {
        FinalNewlinePolicy::Preserve => {}
        FinalNewlinePolicy::Insert if !source.ends_with('\n') => source.push('\n'),
        FinalNewlinePolicy::Remove => {
            while source.ends_with('\n') {
                source.pop();
            }
        }
        FinalNewlinePolicy::Insert => {}
    }
    let mut format = text.format().clone();
    format.utf8_bom = match configured.utf8_bom {
        BomPolicy::Preserve => format.utf8_bom,
        BomPolicy::Always => true,
        BomPolicy::Never => false,
    };
    let configured_ending = match configured.line_endings {
        LineEndingPolicy::Preserve => None,
        LineEndingPolicy::Lf => Some(crate::document::LineEnding::Lf),
        LineEndingPolicy::CrLf => Some(crate::document::LineEnding::CrLf),
        LineEndingPolicy::Cr => Some(crate::document::LineEnding::Cr),
    };
    if let Some(ending) = configured_ending {
        format.preferred_line_ending = ending;
        format.line_endings = vec![ending; source.bytes().filter(|byte| *byte == b'\n').count()];
    }
    crate::document::encode_utf8(&source, &format)
}

fn validate_configured_commands(
    commands: &CommandRegistry<AppEffect>,
    loaded: &LoadedConfig,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let command_ids = commands
        .specs()
        .map(|spec| spec.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    for spec in commands.specs() {
        match loaded.config.command_labels.get(spec.id.as_str()) {
            Some(label)
                if !label.is_empty() && !label.contains('\u{1b}') && label.len() <= 1_024 => {}
            Some(_) => errors.push(format!(
                "command label for `{}` is empty, unsafe, or oversized",
                spec.id
            )),
            None => errors.push(format!("missing command label for `{}`", spec.id)),
        }
    }
    for extra in loaded
        .config
        .command_labels
        .keys()
        .filter(|id| !command_ids.contains(*id))
    {
        errors.push(format!(
            "command label references unknown command `{extra}`"
        ));
    }
    let source = include_str!("interactive.rs");
    let marker = concat!(".", "message(");
    let mut remainder = source;
    while let Some(index) = remainder.find(marker) {
        remainder = &remainder[index + marker.len()..];
        let trimmed = remainder.trim_start();
        if let Some(trimmed) = trimmed.strip_prefix('"')
            && let Some(end) = trimmed.find('"')
        {
            let key = &trimmed[..end];
            if !loaded.config.messages.contains_key(key) {
                errors.push(format!(
                    "missing message `{key}` used by application surface"
                ));
            }
        }
    }
    for (kind, configured) in [
        ("text", &loaded.config.input.text_commands),
        ("paste", &loaded.config.input.paste_commands),
        (
            "character fallback",
            &loaded.config.input.character_fallback_commands,
        ),
    ] {
        for (context, command_id) in configured {
            let invocation =
                CommandInvocation::new(command_id, InvocationSource::Startup).map(|invocation| {
                    invocation.with_argument("text", CommandValue::String(String::new()))
                });
            if let Err(error) =
                invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
            {
                errors.push(format!("{kind} command for `{context}`: {error}"));
            }
        }
    }
    for (alias, command_id) in &loaded.config.input.command_aliases {
        if loaded
            .config
            .input
            .disabled_command_aliases
            .iter()
            .any(|disabled| disabled == alias)
        {
            continue;
        }
        if alias.is_empty() || alias.chars().any(char::is_whitespace) || alias.contains('.') {
            errors.push(format!("invalid command alias `{alias}`"));
            continue;
        }
        let invocation = CommandInvocation::new(command_id, InvocationSource::Startup);
        if let Err(error) = invocation {
            errors.push(format!("command alias `{alias}`: {error}"));
        } else if !commands.specs().any(|spec| spec.id.as_str() == command_id) {
            errors.push(format!(
                "command alias `{alias}` references unknown command `{command_id}`"
            ));
        }
    }
    for (context, command_id) in &loaded.config.input.unmatched_commands {
        let invocation = CommandInvocation::new(command_id, InvocationSource::Startup);
        if let Err(error) =
            invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
        {
            errors.push(format!("unmatched command for `{context}`: {error}"));
        }
    }
    for action in &loaded.dashboard.action {
        let invocation = CommandInvocation::new(&action.command, InvocationSource::Startup)
            .and_then(|mut invocation| {
                for (name, value) in &action.arguments {
                    let value = configuration_value(value).map_err(|()| {
                        CommandError::InvalidArguments(format!(
                            "dashboard argument `{name}` has unsupported type"
                        ))
                    })?;
                    invocation.arguments.insert(name.clone(), value);
                }
                Ok(invocation)
            });
        if let Err(error) =
            invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
        {
            errors.push(format!("dashboard action `{}`: {error}", action.label));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn configuration_value(value: &toml::Value) -> Result<CommandValue, ()> {
    match value {
        toml::Value::String(value) => Ok(CommandValue::String(value.clone())),
        toml::Value::Boolean(value) => Ok(CommandValue::Bool(*value)),
        toml::Value::Integer(value) => Ok(CommandValue::Integer(*value)),
        _ => Err(()),
    }
}

fn invocation_fingerprint(invocation: &CommandInvocation) -> String {
    let mut fingerprint = invocation.id.as_str().to_owned();
    for (name, value) in &invocation.arguments {
        fingerprint.push('|');
        fingerprint.push_str(name);
        fingerprint.push('=');
        match value {
            CommandValue::String(value) => {
                fingerprint.push_str("string:");
                fingerprint.push_str(value);
            }
            CommandValue::Bool(value) => {
                fingerprint.push_str(if *value { "bool:true" } else { "bool:false" })
            }
            CommandValue::Integer(value) => {
                fingerprint.push_str("integer:");
                fingerprint.push_str(&value.to_string());
            }
        }
    }
    fingerprint
}

pub fn reload_keymaps(
    commands: &CommandRegistry<AppEffect>,
    current: &ResolvedKeymaps,
    candidate: &crate::configuration::LoadedKeymaps,
) -> (ResolvedKeymaps, Option<String>) {
    match compile_keymap_entries(
        commands,
        &candidate.entries,
        candidate.bundled_count,
        &candidate.path,
        &candidate.user_spans,
    ) {
        Ok(compiled) => (compiled, None),
        Err(diagnostics) => (
            current.clone(),
            Some(format!(
                "invalid {}; keeping previous keymaps: {}",
                candidate.path.display(),
                diagnostics.join("; ")
            )),
        ),
    }
}

pub fn compile_keymaps(
    commands: &CommandRegistry<AppEffect>,
    loaded: &crate::configuration::LoadedKeymaps,
) -> (ResolvedKeymaps, Option<String>) {
    match compile_keymap_entries(
        commands,
        &loaded.entries,
        loaded.bundled_count,
        &loaded.path,
        &loaded.user_spans,
    ) {
        Ok(keymaps) => (keymaps, None),
        Err(diagnostics) if loaded.bundled_count > 0 => {
            let bundled = compile_keymap_entries(
                commands,
                &loaded.entries[..loaded.bundled_count],
                loaded.bundled_count,
                &loaded.path,
                &[],
            )
            .expect("bundled keymaps are validated by unit tests");
            (
                bundled,
                Some(format!(
                    "invalid {}; keeping bundled keymaps: {}",
                    loaded.path.display(),
                    diagnostics.join("; ")
                )),
            )
        }
        Err(diagnostics) => (
            ResolvedKeymaps::default(),
            Some(format!(
                "invalid {}: {}",
                loaded.path.display(),
                diagnostics.join("; ")
            )),
        ),
    }
}

fn compile_keymap_entries(
    commands: &CommandRegistry<AppEffect>,
    entries: &[crate::configuration::KeymapEntry],
    bundled_count: usize,
    user_path: &std::path::Path,
    user_spans: &[std::ops::Range<usize>],
) -> Result<ResolvedKeymaps, Vec<String>> {
    let mut result = ResolvedKeymaps::default();
    let mut diagnostics = Vec::new();
    let mut declarations = HashMap::<(String, Vec<KeyEvent>, bool), usize>::new();
    let mut effective_declarations = HashMap::<(String, Vec<KeyEvent>), usize>::new();
    for (index, entry) in entries.iter().enumerate() {
        let context = match configured_context(entry) {
            Ok(context) => context,
            Err(error) => {
                diagnostics.push(format!("keymap {}: {error}", index + 1));
                continue;
            }
        };
        let sequence = match entry
            .keys
            .split_whitespace()
            .map(KeyEvent::from_str)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(sequence) if !sequence.is_empty() => sequence,
            Ok(_) => {
                diagnostics.push(format!("keymap {} has an empty key sequence", index + 1));
                continue;
            }
            Err(error) => {
                diagnostics.push(format!("keymap {}: {error}", index + 1));
                continue;
            }
        };
        let user_declaration = index >= bundled_count;
        if let Some(previous) =
            effective_declarations.insert((context.clone(), sequence.clone()), index)
            && (previous >= bundled_count) != user_declaration
        {
            result.conflicts.push(format!(
                "keymap {} shadows keymap {} for `{}` in `{context}`",
                index + 1,
                previous + 1,
                entry.keys
            ));
        }
        if let Some(previous) =
            declarations.insert((context.clone(), sequence.clone(), user_declaration), index)
        {
            diagnostics.push(format!(
                "keymap {} conflicts with keymap {} for `{}` in `{context}`",
                index + 1,
                previous + 1,
                entry.keys
            ));
            continue;
        }
        let operations = usize::from(entry.command.is_some())
            + usize::from(entry.unmap)
            + usize::from(entry.disable)
            + usize::from(entry.pass_through);
        if operations != 1 || !entry.arguments.is_empty() && entry.command.is_none() {
            diagnostics.push(format!(
                "keymap {} must choose exactly one of command, unmap, disable, or pass_through",
                index + 1
            ));
            continue;
        }
        if user_declaration {
            result.reverse.retain(|(mapped_context, _), keys| {
                mapped_context != &context || keys != &entry.keys
            });
            result.inspections.retain(|inspection| {
                inspection.context != context || inspection.keys != entry.keys
            });
        }
        let layer = if user_declaration {
            KeymapLayer::Mode
        } else {
            KeymapLayer::Global
        };
        let keymap = result
            .resolvers
            .entry(context.clone())
            .or_default()
            .layer_mut(layer);
        if entry.unmap || entry.disable {
            keymap.insert_disabled(&sequence);
            result.reverse.retain(|(mapped_context, _), keys| {
                mapped_context != &context || keys != &entry.keys
            });
            result.inspections.retain(|inspection| {
                inspection.context != context || inspection.keys != entry.keys
            });
            continue;
        }
        if entry.pass_through {
            keymap.insert_pass_through(&sequence);
            continue;
        }
        let command_id = entry
            .command
            .as_deref()
            .expect("validated command operation");
        let mut invocation = match CommandInvocation::new(command_id, InvocationSource::Keymap) {
            Ok(invocation) => invocation,
            Err(error) => {
                diagnostics.push(format!("keymap {}: {error}", index + 1));
                continue;
            }
        };
        let mut valid_arguments = true;
        for (name, value) in &entry.arguments {
            let value = match value {
                toml::Value::String(value) => CommandValue::String(value.clone()),
                toml::Value::Boolean(value) => CommandValue::Bool(*value),
                toml::Value::Integer(value) => CommandValue::Integer(*value),
                _ => {
                    diagnostics.push(format!(
                        "keymap {} argument `{name}` must be string, boolean, or integer",
                        index + 1
                    ));
                    valid_arguments = false;
                    continue;
                }
            };
            invocation.arguments.insert(name.clone(), value);
        }
        if !valid_arguments {
            continue;
        }
        if let Err(error) = commands.validate_invocation_shape(&invocation) {
            diagnostics.push(format!("keymap {}: {error}", index + 1));
            continue;
        }
        let fingerprint = invocation_fingerprint(&invocation);
        result.inspections.push(KeymapInspection {
            context: context.clone(),
            command: invocation.id.as_str().into(),
            arguments: invocation.arguments.clone(),
            keys: entry.keys.clone(),
            source: if user_declaration {
                let declaration = index + 1 - bundled_count;
                user_spans.get(declaration - 1).map_or_else(
                    || format!("{}:declaration {declaration}", user_path.display()),
                    |span| {
                        format!(
                            "{}:bytes {}..{} (declaration {declaration})",
                            user_path.display(),
                            span.start,
                            span.end
                        )
                    },
                )
            } else {
                format!("<bundled:keymaps>:declaration {}", index + 1)
            },
        });
        keymap.insert(&sequence, invocation);
        result
            .reverse
            .insert((context, fingerprint), entry.keys.clone());
    }
    if diagnostics.is_empty() {
        Ok(result)
    } else {
        Err(diagnostics)
    }
}

const fn tool_kind_key(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Lsp => "lsp",
        ToolKind::Dap => "dap",
        ToolKind::Formatter => "formatter",
        ToolKind::Linter => "linter",
    }
}

fn configured_context(entry: &crate::configuration::KeymapEntry) -> Result<String, String> {
    const SURFACES: &[&str] = &[
        "dashboard",
        "command_prompt",
        "search_prompt",
        "workspace_trust_prompt",
        "completion",
        "picker",
        "workspace_edit",
        "explorer",
        "panel",
        "terminal",
        "focus",
        "editor_mouse",
        "explorer_mouse",
        "dashboard_mouse",
        "prompt_mouse",
        "tool_manager",
        "tool_manager_mouse",
        "picker_mouse",
        "key_release",
        "register",
        "prefix",
        "replace",
        "macro_record",
        "macro_replay",
    ];
    if entry.context == "editor" {
        let mode = match entry.mode.as_str() {
            "normal" | "insert" | "replace" | "visual_char" | "visual_line" | "visual_block"
            | "operator_pending" => entry.mode.as_str(),
            "visual" => "visual_char",
            "operator" => "operator_pending",
            unknown => return Err(format!("unknown editor mode `{unknown}`")),
        };
        Ok(format!("editor.{mode}"))
    } else if SURFACES.contains(&entry.context.as_str()) {
        if entry.mode.is_empty() {
            Ok(entry.context.clone())
        } else {
            Err(format!(
                "context `{}` does not accept mode `{}`",
                entry.context, entry.mode
            ))
        }
    } else {
        Err(format!("unknown context `{}`", entry.context))
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
    Encoding(#[from] crate::document::EncodingError),
}

fn semantic_role(name: &str) -> crate::syntax::SyntaxRole {
    match name {
        "comment" => crate::syntax::SyntaxRole::Comment,
        "string" => crate::syntax::SyntaxRole::String,
        "number" => crate::syntax::SyntaxRole::Number,
        "keyword" | "modifier" | "operator" => crate::syntax::SyntaxRole::Keyword,
        "type" | "class" | "enum" | "interface" | "struct" => crate::syntax::SyntaxRole::Type,
        _ => crate::syntax::SyntaxRole::Variable,
    }
}

fn make_logs(config: &crate::configuration::Config, persist: bool) -> StructuredLogs {
    let policy = &config.observability;
    StructuredLogs::new(
        LogPolicy {
            maximum_records: policy.maximum_records,
            maximum_field_bytes: policy.maximum_field_bytes,
            maximum_file_bytes: policy.maximum_file_bytes,
            rotations: policy.rotations,
            sensitive_keys: policy.sensitive_keys.clone(),
        },
        (persist && policy.enabled)
            .then(|| crate::configuration::user_state_directory().join(&policy.file_name)),
        policy.queue_capacity,
    )
}

fn task_plan(
    name: &str,
    tasks: &BTreeMap<String, crate::configuration::TaskConfig>,
) -> Option<Vec<String>> {
    fn visit(
        name: &str,
        tasks: &BTreeMap<String, crate::configuration::TaskConfig>,
        seen: &mut BTreeSet<String>,
        plan: &mut Vec<String>,
    ) -> Option<()> {
        if !seen.insert(name.into()) {
            return Some(());
        }
        let task = tasks.get(name)?;
        for dependency in &task.dependencies {
            visit(dependency, tasks, seen, plan)?;
        }
        plan.push(name.into());
        Some(())
    }
    let mut seen = BTreeSet::new();
    let mut plan = Vec::new();
    visit(name, tasks, &mut seen, &mut plan)?;
    Some(plan)
}

fn mode_name_large(mode: LargeFileOverride) -> &'static str {
    match mode {
        LargeFileOverride::Auto => "auto",
        LargeFileOverride::On => "on",
        LargeFileOverride::Off => "off",
    }
}

fn build_restored_layout(
    state: &mut EditorState,
    layout: &PersistedLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::editing::LayoutNode,
    Vec<(PathBuf, crate::editing::PaneId)>,
)> {
    match layout {
        PersistedLayout::Pane { pane } => {
            let (canonical, text) = loaded.get(&pane.path)?;
            let buffer = state.load_hidden_text(text.clone(), canonical.clone());
            let mut view = crate::editing::View::new(buffer);
            let length = state.buffers[buffer].text.len_bytes();
            view.cursor = ByteOffset(pane.cursor.min(length));
            view.scroll_line = pane
                .scroll_line
                .min(state.buffers[buffer].text.len_lines().saturating_sub(1));
            if pane.anchor != pane.cursor {
                view.selection = Some(crate::editing::Selection {
                    anchor: ByteOffset(pane.anchor.min(length)),
                    head: view.cursor,
                    kind: crate::editing::SelectionKind::Character,
                });
            }
            let view = state.views.insert(view);
            let pane_id = state.panes.insert(crate::editing::Pane { view });
            Some((
                crate::editing::LayoutNode::Pane(pane_id),
                vec![(pane.path.clone(), pane_id)],
            ))
        }
        PersistedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !(1..=9999).contains(ratio) {
                return None;
            }
            let (first, mut panes) = build_restored_layout(state, first, loaded)?;
            let (second, second_panes) = build_restored_layout(state, second, loaded)?;
            panes.extend(second_panes);
            let axis = match axis.as_str() {
                "horizontal" => crate::editing::Axis::Horizontal,
                "vertical" => crate::editing::Axis::Vertical,
                _ => return None,
            };
            Some((
                crate::editing::LayoutNode::Split {
                    axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                },
                panes,
            ))
        }
    }
}

fn collect_persisted_panes(layout: &PersistedLayout, panes: &mut Vec<PersistedPane>) {
    match layout {
        PersistedLayout::Pane { pane } => panes.push(pane.clone()),
        PersistedLayout::Split { first, second, .. } => {
            collect_persisted_panes(first, panes);
            collect_persisted_panes(second, panes);
        }
    }
}

fn capture_persisted_layout(
    state: &EditorState,
    layout: &crate::editing::LayoutNode,
) -> Option<PersistedLayout> {
    match layout {
        crate::editing::LayoutNode::Pane(pane) => {
            let pane = state.panes.get(*pane)?;
            let view = state.views.get(pane.view)?;
            let buffer = state.buffers.get(view.buffer)?;
            if buffer.dirty {
                return None;
            }
            let path = buffer.path.clone()?;
            Some(PersistedLayout::Pane {
                pane: PersistedPane {
                    path,
                    cursor: view.cursor.get(),
                    anchor: view
                        .selection
                        .map_or(view.cursor, |selection| selection.anchor)
                        .get(),
                    scroll_line: view.scroll_line,
                },
            })
        }
        crate::editing::LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = capture_persisted_layout(state, first);
            let second = capture_persisted_layout(state, second);
            match (first, second) {
                (Some(first), Some(second)) => Some(PersistedLayout::Split {
                    axis: match axis {
                        crate::editing::Axis::Horizontal => "horizontal",
                        crate::editing::Axis::Vertical => "vertical",
                    }
                    .into(),
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(layout), None) | (None, Some(layout)) => Some(layout),
                (None, None) => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        fs,
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };

    use crate::document::ByteOffset;
    use crate::editing::{CommandValue, InvocationSource};
    use crate::input::KeyEvent;
    use tempfile::tempdir;

    use super::{
        InteractiveApplication, compile_keymaps, make_logs, reload_keymaps,
        validate_configured_commands,
    };
    use crate::application::{StartupTarget, build_state, register_app_commands};
    use crate::{
        observability::CrashSnapshotWriter,
        packages::{PackageService, RegistryService},
        persistence::PersistenceService,
        plugins::PluginManager,
        processes::{ExecutionPreflightService, JobSupervisor, PtyService},
        syntax::SyntaxService,
    };

    fn app() -> InteractiveApplication {
        let root = tempdir().expect("tempdir").keep();
        let startup = StartupTarget::Dashboard { root };
        let state = build_state(&startup);
        let mut commands = crate::editing::CommandRegistry::default();
        crate::editing::register_builtin_commands(&mut commands).expect("editing commands");
        register_app_commands(&mut commands).expect("app commands");
        let loaded = crate::configuration::load_bundled_config();
        let (configuration, diagnostic) =
            super::resolve_configuration(&commands, loaded, true).expect("configuration");
        assert!(diagnostic.is_none());
        let syntax_queue_capacity = configuration.settings.syntax.queue_capacity;
        let persistence_queue_capacity = configuration.settings.persistence.queue_capacity;
        let process_event_capacity = configuration.settings.processes.event_capacity;
        let recovery_directory = state.workspace_root.join("recovery");
        let persistence_path = recovery_directory.join("session.json");
        let trust_state_path = recovery_directory.join("trusted-workspaces.toml");
        let workspace = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [(
                "workspace".into(),
                crate::workspace::RootIdentity {
                    canonical_path: state.workspace_root.clone(),
                    filesystem_identity: None,
                },
            )],
        )
        .expect("workspace");
        let logs = make_logs(&configuration.settings, false);
        let plugin_queue_capacity = configuration.settings.plugins.queue_capacity;
        let pty_queue_capacity = configuration.settings.terminals.queue_capacity;
        let diagnostics = crate::workspace::DiagnosticStore::new(
            configuration.settings.workspace.maximum_diagnostics,
            configuration
                .settings
                .workspace
                .maximum_diagnostics_per_owner_uri,
        );
        InteractiveApplication {
            state,
            workspace,
            diagnostics,
            picker: None,
            picker_payloads: Default::default(),
            pending_workspace_proposal: None,
            workspace_proposal: None,
            workspace_proposal_response: None,
            workspace_resource_intents: Vec::new(),
            accepted_resource_intents: Vec::new(),
            resource_apply_pending: HashMap::new(),
            next_proposal: 1,
            commands,
            input: crate::input::ModalResolver::default(),
            configuration,
            pending_keymap: None,
            ui: crate::terminal_ui::UiState {
                dashboard: true,
                ..Default::default()
            },
            configuration_worker: crate::configuration::ConfigurationWorker::start(
                false,
                crate::configuration::ConfigWatchConfig::default(),
                "dust".into(),
            ),
            configuration_generation: 0,
            vfs: crate::filesystem::VfsWorker::start(2),
            pending_vfs: Default::default(),
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
            terminal_width: 80,
            terminal_height: 24,
            mouse_position: None,
            mouse_selection_anchor: None,
            observed_notification: None,
            notification_deadline: None,
            completion_generation: 0,
            startup_reads: Default::default(),
            startup_reads_pending: 0,
            startup_primary_buffer: None,
            tools: crate::packages::inspect_tools(),
            tool_installer: crate::packages::ToolInstaller::start(),
            lsp: crate::languages::LspService::new(),
            active_lsp_server: None,
            lsp_sessions: HashMap::new(),
            lsp_session_synced: HashMap::new(),
            lsp_synced_revisions: Default::default(),
            lsp_capabilities: None,
            lsp_request_generation: 0,
            lsp_logs: Default::default(),
            lsp_completion_items: Default::default(),
            snippet_session: None,
            lsp_selection_ranges: Vec::new(),
            lsp_semantic_raw: HashMap::new(),
            lsp_pull_ids: HashMap::new(),
            lsp_projection_owner: None,
            lsp_request_owner: None,
            dap: crate::debugging::DapService::new(),
            dap_responses: VecDeque::new(),
            dap_console: VecDeque::new(),
            dap_breakpoints: BTreeMap::new(),
            dap_watches: Vec::new(),
            active_debug_configuration: None,
            active_debug_terminal_profile: None,
            dap_terminal_responses: HashMap::new(),
            recovery_directory,
            recovery_records: Vec::new(),
            recovery_revisions: std::collections::HashMap::new(),
            recovery_pruned_revisions: std::collections::HashMap::new(),
            recovery_scheduled: std::collections::HashMap::new(),
            recovery_discovery_queued: false,
            recovery_cleanup_queued: false,
            terminal_capabilities: "test".into(),
            next_transaction: 1_000_000,
            workspace_search: crate::filesystem::WorkspaceSearchService::default(),
            workspace_search_generation: None,
            workspace_search_matches: Vec::new(),
            workspace_search_intent: None,
            trust_service: crate::workspace::WorkspaceTrustService::start(4),
            trust_store: crate::workspace::TrustStore::default(),
            trust_state_path,
            trust_load_generation: None,
            trust_identity_generation: None,
            trust_persist_generation: None,
            trust_persist_target: None,
            trust_database_ready: false,
            trust_identity_ready: false,
            execution_validation: None,
            execution_validation_generations: std::collections::HashMap::new(),
            execution_revalidated: false,
            safe_mode: false,
            pending_root_operation: None,
            workspace_search_root_revision: None,
            workspace_proposal_root_revision: None,
            directory_service: crate::filesystem::DirectoryService::default(),
            explorer: crate::workspace::ExplorerState::default(),
            explorer_pending: None,
            explorer_hidden: false,
            explorer_ignored: false,
            explorer_preview_path: None,
            explorer_preview_lines: Vec::new(),
            explorer_preview_generation: 0,
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
                crate::configuration::user_state_directory().join("live-health.json"),
            ),
            next_crash_snapshot: Instant::now(),
            plugins: PluginManager::start(plugin_queue_capacity),
            registry: RegistryService::start(
                crate::configuration::PackageRegistryConfig::default(),
                crate::configuration::user_state_directory().join("packages/test-registry-cache"),
            )
            .expect("registry"),
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
                crate::configuration::user_state_directory().join("packages"),
                8,
                16 * 1024 * 1024,
                64 * 1024 * 1024,
            ),
            ptys: PtyService::start(pty_queue_capacity),
            terminals: HashMap::new(),
            active_terminal: None,
            execution_preflight: ExecutionPreflightService::start(process_event_capacity),
            pending_execution_preflight: HashMap::new(),
        }
    }

    fn wait_vfs(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.pending_vfs.is_empty() {
            app.process_vfs_results();
            assert!(Instant::now() < deadline, "VFS completion timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn prepare_trust_identity(app: &mut InteractiveApplication) {
        let identity =
            crate::workspace::RootIdentity::capture(&app.state.workspace_root).expect("identity");
        let root = app.workspace.active_root().expect("root");
        app.workspace
            .replace_root_identity(root, identity)
            .expect("replace identity");
        app.trust_database_ready = true;
        app.trust_identity_ready = true;
        app.reconcile_workspace_trust();
    }

    fn wait_trust(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.trust_persist_generation.is_some() {
            app.process_trust_events();
            assert!(Instant::now() < deadline, "trust persistence timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn workspace_trust_requires_exact_confirmation_cancels_on_focus_loss_and_persists() {
        let mut app = app();
        prepare_trust_identity(&mut app);
        app.request_workspace_trust();
        assert_eq!(
            app.ui.prompt.as_ref().map(|prompt| prompt.kind),
            Some(crate::terminal_ui::PromptKind::WorkspaceTrust)
        );
        app.ui.prompt.as_mut().expect("prompt").input = "wrong".into();
        app.submit_prompt();
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        assert!(app.search_history.is_empty() && app.command_history.is_empty());

        app.request_workspace_trust();
        app.handle_event(crate::terminal_ui::UiEvent::FocusLost);
        assert!(app.ui.prompt.is_none());
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );

        app.request_workspace_trust();
        app.ui.prompt.as_mut().expect("prompt").input = app
            .configuration
            .settings
            .workspace
            .trust_confirmation_phrase
            .clone();
        app.submit_prompt();
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        wait_trust(&mut app);
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Trusted
        );
        let source = fs::read_to_string(&app.trust_state_path).expect("persisted trust");
        assert!(source.contains("filesystem_identity"));

        app.revoke_workspace_trust();
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        wait_trust(&mut app);
        let source = fs::read_to_string(&app.trust_state_path).expect("persisted revocation");
        assert!(!source.contains("filesystem_identity"));
    }

    #[test]
    fn workspace_trust_never_activates_before_identity_and_database_are_ready() {
        let mut app = app();
        app.confirm_workspace_trust(
            app.configuration
                .settings
                .workspace
                .trust_confirmation_phrase
                .clone(),
        );
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        assert!(app.ui.notification_error);
    }

    fn wait_explorer(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.explorer_pending.is_some() {
            app.process_explorer_result();
            assert!(Instant::now() < deadline, "explorer timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn explorer_lazily_loads_and_opens_scoped_file() {
        let mut app = app();
        let path = app.state.workspace_root.join("explorer.txt");
        fs::write(&path, "explorer content\n").expect("fixture");
        app.open_explorer();
        app.explorer_toggle();
        wait_explorer(&mut app);
        assert_eq!(app.explorer.visible().len(), 2);
        app.explorer.move_selection(1);
        app.explorer_open_file(false);
        wait_vfs(&mut app);
        let buffer = app.state.active_buffer_id().expect("opened");
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
        app.open_explorer();
        app.explorer_open_file(true);
        wait_vfs(&mut app);
        assert_eq!(
            super::application_layout_pane_count(
                &app.state.tabs[app.state.active_tab.expect("active tab")].root,
            ),
            2
        );
    }

    fn wait_root_operation(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.pending_root_operation.is_some() {
            app.process_trust_events();
            assert!(Instant::now() < deadline, "root operation timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn async_root_add_picker_activation_and_dirty_removal_preflight_are_real() {
        let mut app = app();
        let first = app.workspace.active_root().expect("first root");
        let second_directory = tempdir().expect("second root");
        let second_path = fs::canonicalize(second_directory.path()).expect("canonical");
        app.add_workspace_root(second_path.clone());
        wait_root_operation(&mut app);
        assert_eq!(app.workspace.roots().len(), 2);
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Untrusted
        );
        let second = app.workspace.active_root().expect("added root active");
        app.activate_workspace_root(first);
        app.open_builtin_picker(crate::application::PickerKind::Roots);
        app.picker_insert(second_path.file_name().unwrap().to_str().unwrap());
        app.accept_picker();
        assert_eq!(app.workspace.active_root(), Some(second));

        let dirty_path = second_path.join("dirty.txt");
        let (buffer, _) = app.state.open_text(
            crate::document::TextBuffer::from_text("dirty"),
            Some(dirty_path),
        );
        app.state.buffers[buffer].dirty = true;
        app.remove_workspace_root(second);
        assert_eq!(app.workspace.roots().len(), 2);
        assert!(app.ui.notification_error);
        app.state.buffers[buffer].dirty = false;
        app.remove_workspace_root(second);
        assert_eq!(app.workspace.roots().len(), 1);
    }

    #[test]
    fn stale_async_root_add_and_pending_service_removal_are_rejected() {
        let mut app = app();
        let second = tempdir().expect("second");
        let third = tempdir().expect("third");
        app.add_workspace_root(second.path().to_path_buf());
        app.workspace
            .add_root(
                "third".into(),
                crate::workspace::RootIdentity::capture(third.path()).expect("identity"),
            )
            .expect("concurrent model mutation");
        wait_root_operation(&mut app);
        assert_eq!(
            app.workspace.roots().len(),
            2,
            "stale async add did not commit"
        );
        let third_id = app.workspace.active_root().expect("third active");
        app.workspace_search_generation = Some(99);
        app.remove_workspace_root(third_id);
        assert_eq!(app.workspace.roots().len(), 2);
        assert!(app.ui.notification_error);
    }

    fn wait_execution_validation(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.execution_validation.is_some() {
            app.process_trust_events();
            assert!(Instant::now() < deadline, "execution validation timed out");
            thread::yield_now();
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_session_root_replacement_cancels_all_protected_action_kinds() {
        let actions = [
            super::DeferredExecution::InstallTool("missing".into()),
            super::DeferredExecution::StartLsp(Some("missing".into())),
            super::DeferredExecution::StartDap {
                requested: Some("missing".into()),
                program: None,
            },
        ];
        for action in actions {
            let mut app = app();
            prepare_trust_identity(&mut app);
            for root in app.workspace.roots() {
                app.trust_store.trust(root.identity.clone());
            }
            app.reconcile_workspace_trust();
            assert_eq!(
                app.workspace.trust(),
                crate::workspace::WorkspaceTrust::Trusted
            );
            let original = app.state.workspace_root.clone();
            let displaced = original.with_extension("displaced");
            fs::rename(&original, &displaced).expect("displace root");
            fs::create_dir(&original).expect("replacement root");
            app.defer_trusted_execution(action);
            wait_execution_validation(&mut app);
            assert_eq!(
                app.workspace.trust(),
                crate::workspace::WorkspaceTrust::Untrusted
            );
            assert!(app.active_lsp_server.is_none());
            assert!(app.ui.notification_error);
        }
    }

    #[test]
    fn protected_action_resumes_once_after_successful_identity_revalidation() {
        let mut app = app();
        prepare_trust_identity(&mut app);
        for root in app.workspace.roots() {
            app.trust_store.trust(root.identity.clone());
        }
        app.reconcile_workspace_trust();
        app.defer_trusted_execution(super::DeferredExecution::InstallTool("missing".into()));
        app.defer_trusted_execution(super::DeferredExecution::StartLsp(None));
        assert!(
            app.ui.notification_error,
            "concurrent protected action is rejected"
        );
        wait_execution_validation(&mut app);
        assert_eq!(
            app.workspace.trust(),
            crate::workspace::WorkspaceTrust::Trusted
        );
        assert!(app.execution_validation_generations.is_empty());
        assert!(app.execution_validation.is_none());
    }

    fn wait_workspace_search(app: &mut InteractiveApplication) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.workspace_search_generation.is_some() || app.pending_workspace_proposal.is_some()
        {
            app.process_workspace_search_events();
            app.process_vfs_results();
            assert!(Instant::now() < deadline, "workspace search timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn workspace_search_uses_generic_picker_and_opens_unopened_match() {
        let mut app = app();
        let path = app.state.workspace_root.join("match.txt");
        fs::write(&path, "before needle after\n").expect("fixture");
        app.start_workspace_search("needle".into(), None);
        wait_workspace_search(&mut app);
        assert_eq!(app.workspace_search_matches.len(), 1);
        assert_eq!(
            app.ui.picker.as_ref().map(|picker| picker.title.as_str()),
            Some("Workspace search")
        );
        app.accept_picker();
        wait_vfs(&mut app);
        let buffer = app.state.active_buffer_id().expect("opened result");
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
        let view = app.state.active_view.expect("view");
        assert_eq!(app.state.views[view].cursor, ByteOffset(7));
    }

    #[test]
    fn workspace_replacement_previews_without_disk_mutation_and_checked_save_rejects_conflict() {
        let mut app = app();
        let first = app.state.workspace_root.join("a.txt");
        let second = app.state.workspace_root.join("b.txt");
        fs::write(&first, "needle one\n").expect("first");
        fs::write(&second, "two needle\n").expect("second");
        app.start_workspace_search("needle".into(), Some("changed".into()));
        wait_workspace_search(&mut app);
        let proposal = app.workspace_proposal.as_ref().expect("preview");
        assert_eq!(proposal.files.len(), 2);
        assert_eq!(
            fs::read_to_string(&first).expect("unchanged"),
            "needle one\n"
        );
        assert_eq!(
            fs::read_to_string(&second).expect("unchanged"),
            "two needle\n"
        );

        fs::write(&first, "external\n").expect("external change");
        app.accept_workspace_proposal();
        assert_eq!(
            fs::read_to_string(&first).expect("still external"),
            "external\n"
        );
        assert_eq!(
            fs::read_to_string(&second).expect("still source"),
            "two needle\n"
        );
        let (first_buffer, _) = app
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&first))
            .expect("hidden first buffer");
        app.state.show_buffer(first_buffer).expect("show first");
        app.start_save(false, None, false);
        wait_vfs(&mut app);
        assert_eq!(
            fs::read_to_string(&first).expect("conflict preserved"),
            "external\n"
        );
        assert!(app.ui.notification_error);
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("externally modified"))
        );
    }

    #[test]
    fn workspace_replacement_rejects_open_buffer_that_differs_from_search_snapshot() {
        let mut app = app();
        let path = app.state.workspace_root.join("stale.txt");
        fs::write(&path, "needle\n").expect("disk");
        let (buffer, _) = app.state.open_text(
            crate::document::TextBuffer::from_text("prefix needle\n"),
            Some(path),
        );
        app.state.buffers[buffer].dirty = true;
        app.start_workspace_search("needle".into(), Some("changed".into()));
        wait_workspace_search(&mut app);
        assert!(app.workspace_proposal.is_none());
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            "prefix needle\n"
        );
        assert!(app.ui.notification_error);
    }

    #[test]
    fn save_as_reload_close_and_external_change_protection_work() {
        let mut app = app();
        let root = app.state.workspace_root.clone();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abc"), None);
        app.ui.dashboard = false;
        app.start_save(false, Some(PathBuf::from("new.txt")), false);
        wait_vfs(&mut app);
        let path = root.join("new.txt");
        assert_eq!(fs::read(&path).expect("saved"), b"abc");
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));

        fs::write(&path, b"external").expect("external edit");
        app.invoke_insert_text("X".into());
        app.start_save(false, None, false);
        wait_vfs(&mut app);
        assert_eq!(fs::read(&path).expect("not overwritten"), b"external");
        assert!(app.ui.notification_error);
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("externally modified"))
        );

        app.start_reload(false);
        assert!(app.pending_vfs.is_empty());
        app.start_reload(true);
        wait_vfs(&mut app);
        assert_eq!(app.state.buffers[buffer].text.to_string(), "external");
        assert!(!app.state.buffers[buffer].dirty);
        app.close_active_buffer(false);
        assert!(app.ui.dashboard);
        assert!(app.state.active_view.is_none());
    }

    #[test]
    fn dirty_quit_is_blocked_but_forced_quit_succeeds() {
        let mut app = app();
        let (_, _) = app
            .state
            .open_text(crate::document::TextBuffer::from_text("dirty"), None);
        let buffer = app.state.active_buffer_id().expect("buffer");
        app.state.buffers[buffer].dirty = true;
        let quit = crate::editing::CommandInvocation::new("editor.quit", InvocationSource::Test)
            .expect("quit")
            .with_argument("force", CommandValue::Bool(false));
        app.invoke(quit);
        assert!(!app.should_quit);
        let force = crate::editing::CommandInvocation::new("editor.quit", InvocationSource::Test)
            .expect("quit")
            .with_argument("force", CommandValue::Bool(true));
        app.invoke(force);
        assert!(app.should_quit);
    }

    #[test]
    fn invalid_configured_regex_keeps_search_prompt_input() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abc"), None);
        app.ui.dashboard = false;
        app.configuration.settings.search.regex = true;
        app.open_prompt(crate::terminal_ui::PromptKind::SearchForward, "[".into());
        app.submit_prompt();
        let prompt = app.ui.prompt.expect("invalid regex prompt remains open");
        assert_eq!(prompt.input, "[");
        assert!(prompt.error.is_some());
    }

    #[test]
    fn search_moves_forward_wraps_and_repeats() {
        let mut app = app();
        let (_, view) = app
            .state
            .open_text(crate::document::TextBuffer::from_text("one two one"), None);
        app.ui.dashboard = false;
        app.ui.search_query = Some("one".into());
        app.perform_search(true);
        assert_eq!(app.state.views[view].cursor, crate::document::ByteOffset(8));
        app.perform_search(true);
        assert_eq!(app.state.views[view].cursor, crate::document::ByteOffset(0));
        app.perform_search(false);
        assert_eq!(app.state.views[view].cursor, crate::document::ByteOffset(8));
    }

    #[test]
    fn untrusted_workspace_blocks_project_local_language_server_launch() {
        let mut app = app();
        let path = app.state.workspace_root.join("main.rs");
        app.state.open_text(
            crate::document::TextBuffer::from_text("fn main() {}"),
            Some(path),
        );
        app.tools.push(crate::packages::ToolEntry {
            definition: crate::packages::ToolDefinition {
                id: "project-local".into(),
                name: "Project local".into(),
                kind: crate::packages::ToolKind::Lsp,
                languages: vec!["rust".into()],
                executable: app
                    .state
                    .workspace_root
                    .join("bin/server")
                    .display()
                    .to_string(),
                launch_args: Vec::new(),
                install_program: None,
                install_args: Vec::new(),
            },
            status: crate::packages::ToolStatus::Installed(
                app.state.workspace_root.join("bin/server"),
            ),
        });
        app.start_lsp(Some("project-local".into()));
        assert!(app.active_lsp_server.is_none());
        assert!(app.ui.notification_error);
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("trust"))
        );
        prepare_trust_identity(&mut app);
        for root in app.workspace.roots() {
            app.trust_store.trust(root.identity.clone());
        }
        app.reconcile_workspace_trust();
        app.start_lsp(Some("project-local".into()));
        wait_execution_validation(&mut app);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.pending_execution_preflight.is_empty() && Instant::now() < deadline {
            app.process_execution_preflight();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.active_lsp_server.is_none()
                && app.ui.notification_error
                && app
                    .ui
                    .notification
                    .as_deref()
                    .is_some_and(|message| !message.is_empty())
        );
    }

    #[test]
    fn generic_picker_filters_and_invokes_registry_commands() {
        let mut app = app();
        app.open_builtin_picker(crate::application::PickerKind::Commands);
        assert!(
            app.ui
                .picker
                .as_ref()
                .is_some_and(|picker| !picker.rows.is_empty())
        );
        app.picker_insert("health");
        let picker = app.ui.picker.as_ref().expect("picker");
        assert!(picker.rows.iter().all(|row| {
            format!("{} {}", row.label, row.detail)
                .to_lowercase()
                .contains("health")
        }));
        app.accept_picker();
        assert!(app.ui.picker.is_none());
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("Dust health"))
        );
    }

    #[test]
    fn unopened_multifile_workspace_edit_previews_then_applies_only_to_editor_buffers() {
        let mut app = app();
        let first = app.state.workspace_root.join("first.rs");
        let second = app.state.workspace_root.join("second.rs");
        fs::write(&first, "abc").expect("first");
        fs::write(&second, "xyz").expect("second");
        let first = fs::canonicalize(first).expect("canonical first");
        let second = fs::canonicalize(second).expect("canonical second");
        let (first_buffer, _) = app.state.open_text(
            crate::document::TextBuffer::from_bytes(b"abc").expect("text"),
            Some(first.clone()),
        );
        let range = |start, end| crate::languages::LspRange {
            start: crate::languages::LspPosition {
                line: 0,
                character: start,
            },
            end: crate::languages::LspPosition {
                line: 0,
                character: end,
            },
        };
        let edit = crate::languages::LspWorkspaceEdit {
            documents: vec![
                crate::languages::LspDocumentEdit {
                    uri: crate::languages::file_uri(&first),
                    version: Some(0),
                    edits: vec![crate::languages::LspTextEdit {
                        range: range(1, 2),
                        new_text: "B".into(),
                    }],
                },
                crate::languages::LspDocumentEdit {
                    uri: crate::languages::file_uri(&second),
                    version: None,
                    edits: vec![crate::languages::LspTextEdit {
                        range: range(1, 2),
                        new_text: "Y".into(),
                    }],
                },
            ],
            resources: Vec::new(),
        };
        app.begin_workspace_proposal(
            "rename preview".into(),
            edit,
            None,
            crate::document::PositionEncoding::Utf16,
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.workspace_proposal.is_none() && Instant::now() < deadline {
            app.process_vfs_results();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(app.workspace_proposal.is_some());
        assert_eq!(app.state.buffers[first_buffer].text.to_string(), "abc");
        assert_eq!(fs::read_to_string(&second).expect("disk"), "xyz");
        app.accept_workspace_proposal();
        assert_eq!(app.state.buffers[first_buffer].text.to_string(), "aBc");
        let second_buffer = app
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&second))
            .map(|(buffer, _)| buffer)
            .expect("hidden second buffer");
        assert_eq!(app.state.buffers[second_buffer].text.to_string(), "xYz");
        assert!(app.state.buffers[first_buffer].dirty && app.state.buffers[second_buffer].dirty);
        assert_eq!(fs::read_to_string(&second).expect("disk remains"), "xyz");
    }

    #[test]
    fn lsp_completion_applies_through_one_validated_transaction() {
        let mut app = app();
        let path = app.state.workspace_root.join("main.rs");
        let (_, view) = app
            .state
            .open_text(crate::document::TextBuffer::from_text("fn ma"), Some(path));
        app.state.views[view].cursor = ByteOffset(5);
        app.ui.dashboard = false;
        app.lsp_capabilities = Some(crate::languages::LspCapabilities {
            position_encoding: crate::document::PositionEncoding::Utf16,
            ..Default::default()
        });
        app.lsp_completion_items = vec![crate::languages::LspCompletionItem {
            label: "main".into(),
            detail: "function".into(),
            insert_text: "in".into(),
            text_edit: None,
            additional_edits: Vec::new(),
            kind: None,
            snippet: false,
            documentation: String::new(),
            resolve_item: None,
            resolved: false,
        }];
        app.ui.language_completion = Some(crate::terminal_ui::LanguageCompletionState {
            items: vec![crate::terminal_ui::CompletionItem {
                label: "main".into(),
                detail: "function".into(),
            }],
            selected: 0,
        });
        app.accept_lsp_completion();
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "fn main");
        assert!(app.ui.language_completion.is_none());
        assert!(app.state.buffers[buffer].dirty);
    }

    #[test]
    fn stale_lsp_workspace_edit_is_rejected_without_mutation() {
        let mut app = app();
        let path = app.state.workspace_root.join("main.rs");
        let (buffer, _) = app.state.open_text(
            crate::document::TextBuffer::from_text("abc"),
            Some(path.clone()),
        );
        let edit = crate::languages::LspWorkspaceEdit {
            documents: vec![crate::languages::LspDocumentEdit {
                uri: crate::languages::file_uri(&path),
                version: Some(99),
                edits: vec![crate::languages::LspTextEdit {
                    range: crate::languages::LspRange {
                        start: crate::languages::LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: crate::languages::LspPosition {
                            line: 0,
                            character: 1,
                        },
                    },
                    new_text: "z".into(),
                }],
            }],
            resources: Vec::new(),
        };
        assert!(app.apply_lsp_workspace_edit(edit).is_err());
        assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    }

    #[test]
    fn lsp_tool_menu_is_registry_driven_filterable_and_configurable() {
        let mut app = app();
        app.process_effect(crate::application::AppEffect::OpenToolMenu("lsp".into()));
        let menu = app.ui.tool_menu.as_ref().expect("tool menu");
        assert_eq!(menu.category, "lsp");
        assert!(menu.rows.iter().all(|row| row.kind == "LSP"));
        assert!(menu.rows.iter().any(|row| row.id == "rust-analyzer"));
        app.handle_configured_key(KeyEvent::character('j'));
        assert_eq!(app.ui.tool_menu.as_ref().expect("menu").selected, 1);
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Tab));
        assert_eq!(app.ui.tool_menu.as_ref().expect("menu").category, "dap");
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Escape));
        assert!(app.ui.tool_menu.is_none());
    }

    #[test]
    fn prompt_mouse_context_is_independently_configurable() {
        let mut app = app();
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.entries.push(crate::configuration::KeymapEntry {
            context: "prompt_mouse".into(),
            mode: String::new(),
            keys: "mouse-right-down".into(),
            command: Some("command_line.cancel".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
        });
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(diagnostic.is_none());
        app.configuration.keymaps = keymaps;
        app.open_prompt(crate::terminal_ui::PromptKind::Command, String::new());
        app.handle_event(crate::terminal_ui::UiEvent::Mouse {
            key: crate::input::KeyEvent::plain(crate::input::KeyCode::MouseRightDown),
            column: 1,
            row: 1,
        });
        assert!(app.ui.prompt.is_none());
    }

    #[test]
    fn normalized_mouse_position_invokes_real_cursor_placement() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abcdef"), None);
        app.ui.dashboard = false;
        app.mouse_position = Some((5, 0));
        app.apply_mouse_cursor(false);
        let view = app.state.active_view.expect("view");
        assert_eq!(app.state.views[view].cursor, crate::document::ByteOffset(2));
        app.mouse_position = Some((7, 0));
        app.apply_mouse_cursor(true);
        let selection = app.state.views[view].selection.expect("mouse selection");
        assert_eq!(selection.anchor, crate::document::ByteOffset(2));
        assert_eq!(selection.head, crate::document::ByteOffset(4));
    }

    #[test]
    fn mouse_click_focuses_the_real_target_pane_before_placing_cursor() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abcdef"), None);
        let first = app.state.active_view.expect("first view");
        app.state
            .split_active(crate::editing::Axis::Horizontal, 500)
            .expect("split");
        assert_ne!(app.state.active_view, Some(first));
        app.mouse_position = Some((4, 0));
        app.apply_mouse_cursor(false);
        assert_eq!(app.state.active_view, Some(first));
        assert_eq!(
            app.state.views[first].cursor,
            crate::document::ByteOffset(1)
        );
    }

    #[test]
    fn configured_file_format_policy_is_applied_at_save_boundary() {
        let text = crate::document::TextBuffer::from_bytes(b"a\r\nb").expect("CRLF text");
        let configured = crate::configuration::EditorConfig {
            line_endings: crate::configuration::LineEndingPolicy::Lf,
            final_newline: crate::configuration::FinalNewlinePolicy::Insert,
            utf8_bom: crate::configuration::BomPolicy::Always,
            ..crate::configuration::EditorConfig::default()
        };
        assert_eq!(
            super::configured_file_bytes(&text, &configured),
            b"\xEF\xBB\xBFa\nb\n"
        );
    }

    #[test]
    fn dirty_layout_subtrees_are_blocked_and_forced_buffer_close_remains_reachable() {
        let mut app = app();
        let (dirty, _) = app
            .state
            .open_text(crate::document::TextBuffer::from_text("dirty"), None);
        app.ui.dashboard = false;
        app.state.buffers[dirty].dirty = true;
        app.state
            .split_active(crate::editing::Axis::Horizontal, 500)
            .expect("shared split");
        app.state.new_tab().expect("shared tab");
        let tab_count = app.state.tabs.len();
        app.apply_layout_action(crate::application::LayoutAction::CloseTab);
        assert_eq!(
            app.state.tabs.len(),
            tab_count,
            "dirty tab close is blocked"
        );
        app.state
            .validate_layout()
            .expect("blocked layout remains valid");

        app.close_active_buffer(true);
        assert!(app.state.buffers.contains_key(dirty));
        assert!(app.state.buffers[dirty].dirty);
        app.state
            .cycle_buffer(1, true)
            .expect("reopen forced-close buffer");
        assert_eq!(app.state.active_buffer_id(), Some(dirty));
        app.state.validate_layout().expect("reopened layout valid");
    }

    #[test]
    fn configured_text_and_dashboard_commands_are_schema_validated() {
        let app = app();
        let mut loaded = crate::configuration::load_bundled_config();
        assert!(validate_configured_commands(&app.commands, &loaded).is_ok());
        loaded
            .config
            .input
            .text_commands
            .insert("editor.insert".into(), "unknown.command".into());
        assert!(
            validate_configured_commands(&app.commands, &loaded)
                .expect_err("invalid command")
                .contains("editor.insert")
        );
    }

    #[test]
    fn command_and_search_histories_have_independent_configured_bounds() {
        let mut app = app();
        app.configuration.settings.history.command.maximum_entries = 1;
        app.configuration.settings.history.search.maximum_entries = 2;
        app.record_prompt_history(crate::terminal_ui::PromptKind::Command, "first");
        app.record_prompt_history(crate::terminal_ui::PromptKind::Command, "second");
        app.record_prompt_history(crate::terminal_ui::PromptKind::SearchForward, "needle");
        assert_eq!(app.command_history, vec!["second"]);
        assert_eq!(app.search_history, vec!["needle"]);
        app.configuration.settings.history.search.enabled = false;
        app.record_prompt_history(crate::terminal_ui::PromptKind::SearchBackward, "ignored");
        assert_eq!(app.search_history, vec!["needle"]);
    }

    #[test]
    fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
        let mut app = app();
        app.configuration.settings.recovery.write_debounce_ms = 10;
        app.recovery_discovery_queued = true;
        app.state
            .open_text(crate::document::TextBuffer::from_text("base"), None);
        app.ui.dashboard = false;
        let buffer = app.state.active_buffer_id().expect("buffer");
        app.invoke_insert_text("A".into());
        app.schedule_recovery_work();
        let first_revision = app.state.buffers[buffer].text.revision();
        app.invoke_insert_text("B".into());
        app.schedule_recovery_work();
        let latest_revision = app.state.buffers[buffer].text.revision();
        assert_ne!(first_revision, latest_revision);
        assert_eq!(app.recovery_scheduled.len(), 1);
        assert_eq!(app.recovery_scheduled[&buffer].0, latest_revision);
        assert!(
            !app.pending_vfs
                .values()
                .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
        );
        std::thread::sleep(Duration::from_millis(12));
        app.schedule_recovery_work();
        wait_vfs(&mut app);
        let journal = app.recovery_path(buffer);
        let recovered = crate::filesystem::discover_recovery(&app.recovery_directory);
        assert_eq!(recovered.records.len(), 1);
        assert_eq!(recovered.records[0].1.revision, latest_revision.0);

        app.state.buffers[buffer].mark_saved(latest_revision);
        app.schedule_recovery_work();
        wait_vfs(&mut app);
        assert!(!journal.exists());
        assert!(!app.recovery_revisions.contains_key(&buffer));

        app.configuration.settings.recovery.write_debounce_ms = 0;
        app.invoke_insert_text("X".into());
        let revision = app.state.buffers[buffer].text.revision();
        app.schedule_recovery_work();
        app.state.buffers[buffer].mark_saved(revision);
        app.schedule_recovery_work();
        wait_vfs(&mut app);
        assert!(!app.recovery_path(buffer).exists());
    }

    #[test]
    fn recovery_delete_completion_cannot_forget_a_newer_queued_write() {
        let mut app = app();
        app.configuration.settings.recovery.write_debounce_ms = 0;
        app.configuration.settings.recovery.cleanup_enabled = false;
        app.recovery_discovery_queued = true;
        app.recovery_cleanup_queued = true;
        app.state
            .open_text(crate::document::TextBuffer::from_text("base"), None);
        app.ui.dashboard = false;
        let buffer = app.state.active_buffer_id().expect("buffer");
        app.invoke_insert_text("A".into());
        app.schedule_recovery_work();
        wait_vfs(&mut app);

        let saved_revision = app.state.buffers[buffer].text.revision();
        app.state.buffers[buffer].mark_saved(saved_revision);
        app.schedule_recovery_work();
        assert!(
            app.pending_vfs
                .values()
                .any(|pending| { matches!(pending, super::PendingVfs::RecoveryDelete { .. }) })
        );

        app.invoke_insert_text("B".into());
        let latest = app.state.buffers[buffer].text.revision();
        app.schedule_recovery_work();
        assert_eq!(app.recovery_revisions.get(&buffer), Some(&latest));
        wait_vfs(&mut app);

        let recovered = crate::filesystem::discover_recovery(&app.recovery_directory);
        assert_eq!(recovered.records.len(), 1);
        assert_eq!(recovered.records[0].1.revision, latest.0);
        assert_eq!(app.recovery_revisions.get(&buffer), Some(&latest));
        app.schedule_recovery_work();
        assert!(
            !app.pending_vfs
                .values()
                .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
        );
    }

    #[test]
    fn retention_pruning_suppresses_only_the_pruned_live_revision_without_churn() {
        let mut app = app();
        app.configuration.settings.recovery.write_debounce_ms = 0;
        app.configuration.settings.recovery.maximum_retained_records = 0;
        app.configuration.settings.recovery.maximum_age_days = 0;
        app.recovery_discovery_queued = true;
        app.recovery_cleanup_queued = true;
        app.state
            .open_text(crate::document::TextBuffer::from_text("base"), None);
        let buffer = app.state.active_buffer_id().expect("buffer");
        app.invoke_insert_text("A".into());
        let pruned_revision = app.state.buffers[buffer].text.revision();
        app.schedule_recovery_work();
        wait_vfs(&mut app);
        app.schedule_recovery_work();
        wait_vfs(&mut app);
        assert!(!app.recovery_path(buffer).exists());
        assert_eq!(
            app.recovery_pruned_revisions.get(&buffer),
            Some(&pruned_revision)
        );
        app.schedule_recovery_work();
        assert!(
            !app.pending_vfs
                .values()
                .any(|pending| { matches!(pending, super::PendingVfs::RecoveryWrite { .. }) })
        );

        app.invoke_insert_text("B".into());
        let newer = app.state.buffers[buffer].text.revision();
        app.schedule_recovery_work();
        assert_ne!(newer, pruned_revision);
        assert!(app.pending_vfs.values().any(|pending| {
            matches!(pending, super::PendingVfs::RecoveryWrite { revision, .. } if *revision == newer)
        }));
    }

    #[test]
    fn disabled_recovery_policy_schedules_no_work() {
        let mut app = app();
        app.configuration.settings.recovery.enabled = false;
        app.state
            .open_text(crate::document::TextBuffer::from_text("base"), None);
        app.invoke_insert_text("X".into());
        app.schedule_recovery_work();
        assert!(app.recovery_scheduled.is_empty());
        assert!(app.pending_vfs.is_empty());
    }

    #[test]
    fn configured_notification_timeout_expires_visible_message() {
        let mut app = app();
        app.configuration.settings.ui.notifications.timeout_ms = 1;
        app.ui.notification = Some("transient".into());
        app.update_notification_timer();
        std::thread::sleep(Duration::from_millis(2));
        app.update_notification_timer();
        assert!(app.ui.notification.is_none());
    }

    #[test]
    fn stale_configuration_generations_are_never_applied() {
        assert!(super::configuration_generation_is_newer(4, 5));
        assert!(!super::configuration_generation_is_newer(5, 5));
        assert!(!super::configuration_generation_is_newer(6, 5));
    }

    #[test]
    fn complete_configuration_reload_is_atomic() {
        let mut app = app();
        let original_theme = app.configuration.theme.clone();
        let explanation = app
            .configuration
            .explain("ui.command_line.width")
            .expect("explanation");
        assert_eq!(explanation.source, "bundled defaults");
        assert_eq!(explanation.contributors, vec!["bundled defaults"]);
        let original_width = app.configuration.settings.ui.command_line.width;
        let mut invalid = crate::configuration::load_bundled_config();
        invalid.config.ui.command_line.width = original_width + 1;
        invalid
            .config
            .input
            .text_commands
            .insert("editor.insert".into(), "unknown.command".into());
        assert!(app.apply_configuration_candidate(invalid, false).is_err());
        assert_eq!(
            app.configuration.settings.ui.command_line.width,
            original_width
        );
        assert_eq!(app.configuration.theme, original_theme);

        let mut valid = crate::configuration::load_bundled_config();
        valid.config.ui.command_line.width = original_width + 1;
        valid.dashboard.identity = "CUSTOM DASH".into();
        app.apply_configuration_candidate(valid, false)
            .expect("valid complete snapshot");
        assert_eq!(
            app.configuration.settings.ui.command_line.width,
            original_width + 1
        );
        assert_eq!(app.configuration.dashboard.identity, "CUSTOM DASH");
    }

    #[test]
    fn resolved_explain_reports_exact_layered_and_replacement_provenance() {
        let app = app();
        let root = tempdir().expect("tempdir");
        let config_path = root.path().join("config.toml");
        let config_source = "editor = { tab_width = 7 }\n";
        fs::write(&config_path, config_source).expect("config");
        let mut loaded = crate::configuration::load_config(&config_path);

        let dashboard_path = root.path().join("dashboard.toml");
        let dashboard_source = include_str!("../../assets/defaults/dashboard.toml")
            .replace("D U S T", "PROVENANCE DASH");
        loaded.dashboard = toml::from_str(&dashboard_source).expect("dashboard");
        loaded.dashboard_path = dashboard_path.clone();
        loaded.dashboard_spans = crate::configuration::setting_spans(&dashboard_source);

        let theme_path = root.path().join("provenance.toml");
        let theme_source = include_str!("../../assets/themes/dust.toml")
            .replace("name = \"dust\"", "name = \"provenance\"");
        loaded.theme = toml::from_str(&theme_source).expect("theme");
        loaded.theme_path = theme_path.clone();
        loaded.theme_spans = crate::configuration::setting_spans(&theme_source);

        let (resolved, diagnostic) =
            super::resolve_configuration(&app.commands, loaded, true).expect("resolve");
        assert!(diagnostic.is_none());
        let setting = resolved.explain("editor.tab_width").expect("setting");
        assert!(
            setting
                .source
                .starts_with(&config_path.display().to_string())
        );
        assert_eq!(setting.contributors[0], "bundled defaults");
        assert_eq!(setting.contributors[1], setting.source);

        let dashboard = resolved
            .explain("dashboard.identity")
            .expect("dashboard identity");
        assert!(
            dashboard
                .source
                .starts_with(&dashboard_path.display().to_string())
        );
        assert_eq!(dashboard.contributors, vec![dashboard.source.clone()]);
        let dashboard_action = resolved
            .explain("dashboard.action.0.label")
            .expect("dashboard action label");
        assert_eq!(
            dashboard_action.contributors,
            vec![dashboard_action.source.clone()]
        );

        let theme = resolved.explain("theme.name").expect("theme name");
        assert!(theme.source.starts_with(&theme_path.display().to_string()));
        assert_eq!(theme.contributors, vec![theme.source.clone()]);

        for (source, explanation, expected) in [
            (config_source, setting, "7"),
            (dashboard_source.as_str(), dashboard, "\"PROVENANCE DASH\""),
            (dashboard_source.as_str(), dashboard_action, "\"Command\""),
            (theme_source.as_str(), theme, "\"provenance\""),
        ] {
            let range = explanation
                .source
                .split_once(":bytes ")
                .and_then(|(_, range)| range.split_once(".."))
                .and_then(|(start, end)| Some(start.parse().ok()?..end.parse().ok()?))
                .expect("byte range");
            assert_eq!(&source[range], expected);
        }
    }

    #[test]
    fn user_keymaps_execute_typed_commands_and_unmap_defaults() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abc"), None);
        app.ui.dashboard = false;
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.path = PathBuf::from("keymaps.toml");
        loaded.entries.extend([
            crate::configuration::KeymapEntry {
                context: "editor".into(),
                mode: "normal".into(),
                keys: "space q".into(),
                command: Some("editor.quit".into()),
                arguments: [("force".into(), toml::Value::Boolean(true))]
                    .into_iter()
                    .collect(),
                unmap: false,
                disable: false,
                pass_through: false,
            },
            crate::configuration::KeymapEntry {
                context: "editor".into(),
                mode: "normal".into(),
                keys: "x".into(),
                command: None,
                arguments: Default::default(),
                unmap: true,
                disable: false,
                pass_through: false,
            },
        ]);
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(diagnostic.is_none());
        assert!(
            keymaps
                .conflicts
                .iter()
                .any(|conflict| conflict.contains("shadows"))
        );
        app.configuration.keymaps = keymaps;
        let inspection = app
            .configuration
            .keymaps
            .inspect("editor.normal", "editor.quit")
            .expect("inspect remap");
        assert_eq!(inspection.keys, "space q");
        assert_eq!(
            inspection.arguments.get("force"),
            Some(&CommandValue::Bool(true))
        );
        app.handle_configured_key(KeyEvent::character('x'));
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
        app.handle_configured_key(KeyEvent::character(' '));
        app.handle_configured_key(KeyEvent::character('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn invalid_user_keymap_set_is_rejected_transactionally() {
        let app = app();
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.path = PathBuf::from("keymaps.toml");
        loaded.entries.push(crate::configuration::KeymapEntry {
            context: "editor".into(),
            mode: "normal".into(),
            keys: "x".into(),
            command: Some("unknown.command".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
        });
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(!keymaps.resolvers.is_empty());
        assert!(diagnostic.is_some_and(|message| message.contains("keeping bundled keymaps")));

        let (reloaded, reload_diagnostic) =
            reload_keymaps(&app.commands, &app.configuration.keymaps, &loaded);
        assert_eq!(
            reloaded.binding_for("editor.normal", "edit.delete_char"),
            app.configuration
                .keymaps
                .binding_for("editor.normal", "edit.delete_char")
        );
        assert!(
            reload_diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("keeping previous keymaps"))
        );
    }

    #[test]
    fn keymap_inspection_reports_real_user_path_and_byte_range() {
        let mut app = app();
        let root = tempdir().expect("tempdir");
        let path = root.path().join("keymaps.toml");
        fs::write(
            &path,
            "[[keymap]]\ncontext='dashboard'\nkeys='z'\ncommand='health.open'\n",
        )
        .expect("keymap");
        let user = crate::configuration::load_keymaps(&path);
        let mut bundled = crate::configuration::load_bundled_config().keymaps;
        let bundled_count = bundled.entries.len();
        bundled.entries.extend(user.entries);
        bundled.bundled_count = bundled_count;
        bundled.path = path.clone();
        bundled.user_spans = user.user_spans;
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &bundled);
        assert!(diagnostic.is_none());
        let source = keymaps
            .inspect_key("dashboard", "z")
            .expect("inspection")
            .source;
        assert!(source.starts_with(&path.display().to_string()));
        assert!(source.contains(":bytes 0.."));
        app.configuration.keymaps = keymaps;
    }

    #[test]
    fn duplicate_user_mappings_report_conflicts_transactionally() {
        let app = app();
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.path = PathBuf::from("keymaps.toml");
        let mapping = crate::configuration::KeymapEntry {
            context: "dashboard".into(),
            mode: String::new(),
            keys: "z".into(),
            command: Some("editor.quit".into()),
            arguments: [("force".into(), toml::Value::Boolean(false))]
                .into_iter()
                .collect(),
            unmap: false,
            disable: false,
            pass_through: false,
        };
        loaded.entries.extend([mapping.clone(), mapping]);
        let (_, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(
            diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("conflicts with keymap"))
        );
    }

    #[test]
    fn configured_focus_loss_cancels_pending_mapping() {
        let mut app = app();
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.entries.push(crate::configuration::KeymapEntry {
            context: "dashboard".into(),
            mode: String::new(),
            keys: "z z".into(),
            command: Some("health.open".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
        });
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(diagnostic.is_none());
        app.configuration.keymaps = keymaps;
        app.handle_configured_key(KeyEvent::character('z'));
        assert!(app.pending_keymap.is_some());
        app.handle_event(crate::terminal_ui::UiEvent::FocusLost);
        assert!(app.pending_keymap.is_none());
    }

    #[test]
    fn prompt_special_keys_are_remappable_from_configuration() {
        let mut app = app();
        let mut loaded = crate::configuration::load_bundled_config().keymaps;
        loaded.entries.extend([
            crate::configuration::KeymapEntry {
                context: "command_prompt".into(),
                mode: String::new(),
                keys: "escape".into(),
                command: None,
                arguments: Default::default(),
                unmap: true,
                disable: false,
                pass_through: false,
            },
            crate::configuration::KeymapEntry {
                context: "command_prompt".into(),
                mode: String::new(),
                keys: "ctrl-c".into(),
                command: Some("command_line.cancel".into()),
                arguments: Default::default(),
                unmap: false,
                disable: false,
                pass_through: false,
            },
        ]);
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded);
        assert!(diagnostic.is_none());
        app.configuration.keymaps = keymaps;
        app.open_prompt(crate::terminal_ui::PromptKind::Command, String::new());
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Escape));
        assert!(app.ui.prompt.is_some());
        app.handle_configured_key("ctrl-c".parse().expect("configured key"));
        assert!(app.ui.prompt.is_none());
    }

    #[test]
    fn first_frame_renders_before_startup_file_is_queued_and_vfs_then_opens_it() {
        let mut app = app();
        let path = app.state.workspace_root.join("startup.txt");
        fs::write(&path, b"startup\n").expect("startup file");
        app.ui.dashboard = false;
        app.startup_reads.push_back((path.clone(), true));
        let frame = crate::terminal_ui::render(
            &app.state,
            &app.ui,
            &app.configuration.settings,
            &app.configuration.dashboard,
            80,
            24,
        );
        assert_eq!(frame.grid.width(), 80);
        assert!(app.pending_vfs.is_empty());
        assert!(app.state.buffers.is_empty());

        app.queue_startup_reads();
        assert!(app.pending_vfs.values().any(|pending| {
            matches!(pending, super::PendingVfs::StartupOpen { primary: true })
        }));
        wait_vfs(&mut app);
        let buffer = app.state.active_buffer_id().expect("startup buffer");
        assert_eq!(
            app.state.buffers[buffer].path.as_deref(),
            Some(path.as_path())
        );
        assert_eq!(app.state.buffers[buffer].text.to_string(), "startup\n");
        assert!(app.startup_reads.is_empty());
        assert_eq!(app.startup_reads_pending, 0);
    }

    #[test]
    fn deferred_startup_additional_files_restore_the_primary_buffer() {
        let mut app = app();
        let primary = app.state.workspace_root.join("primary.txt");
        let additional = app.state.workspace_root.join("additional.txt");
        fs::write(&primary, b"primary").expect("primary");
        fs::write(&additional, b"additional").expect("additional");
        app.startup_reads.push_back((primary.clone(), true));
        app.startup_reads.push_back((additional.clone(), false));
        app.queue_startup_reads();
        wait_vfs(&mut app);
        assert_eq!(app.state.buffers.len(), 2);
        let active = app.state.active_buffer_id().expect("active primary");
        assert_eq!(
            app.state.buffers[active].path.as_deref(),
            Some(primary.as_path())
        );
        assert!(
            app.state
                .buffers
                .iter()
                .any(|(_, buffer)| { buffer.path.as_deref() == Some(additional.as_path()) })
        );
        app.state.validate_layout().expect("valid startup layout");
    }

    #[test]
    fn path_completion_is_bounded_async_and_rejects_stale_generations() {
        let mut app = app();
        fs::write(app.state.workspace_root.join("alpha.txt"), b"a").expect("alpha");
        app.open_prompt(crate::terminal_ui::PromptKind::Command, "e a".into());
        wait_vfs(&mut app);
        assert!(
            app.ui
                .prompt
                .as_ref()
                .expect("prompt")
                .completions
                .iter()
                .any(|item| item.label == "alpha.txt")
        );

        app.prompt_insert("z");
        wait_vfs(&mut app);
        assert!(
            app.ui
                .prompt
                .as_ref()
                .expect("prompt")
                .completions
                .is_empty()
        );
    }

    #[test]
    fn command_completion_accepts_registry_items_and_preserves_invalid_input() {
        let mut app = app();
        app.configuration
            .settings
            .command_labels
            .insert("health.open".into(), "Vitals".into());
        app.open_prompt(crate::terminal_ui::PromptKind::Command, "hea".into());
        let index = app
            .ui
            .prompt
            .as_ref()
            .expect("prompt")
            .completions
            .iter()
            .position(|item| item.label == "health" && item.detail == "Vitals")
            .expect("health completion");
        app.ui.prompt.as_mut().expect("prompt").selected_completion = Some(index);
        app.submit_prompt();
        assert_eq!(app.ui.prompt.as_ref().expect("prompt").input, "health");
        app.prompt_insert("-not-real");
        app.submit_prompt();
        let prompt = app.ui.prompt.as_ref().expect("invalid prompt stays open");
        assert_eq!(prompt.input, "health-not-real");
        assert!(prompt.error.is_some());
    }

    #[test]
    fn completion_selection_and_submit_acceptance_follow_configuration() {
        let mut app = app();
        app.configuration.settings.input.completion_select_first = true;
        app.configuration.settings.input.completion_accept_on_submit = false;
        app.open_prompt(crate::terminal_ui::PromptKind::Command, "hea".into());
        assert_eq!(
            app.ui.prompt.as_ref().expect("prompt").selected_completion,
            Some(0)
        );
        app.submit_prompt();
        let prompt = app.ui.prompt.as_ref().expect("prompt retained");
        assert_eq!(prompt.input, "hea");
        assert!(prompt.error.is_some());
    }

    #[test]
    fn dot_repeats_semantic_insert_session() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("one"), None);
        app.ui.dashboard = false;
        for key in ['i', 'X'] {
            app.handle_configured_key(KeyEvent::character(key));
        }
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Escape));
        app.handle_configured_key(KeyEvent::character('.'));
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "XXone");
        assert_eq!(app.last_change.len(), 3);
    }

    #[test]
    fn normalized_characterwise_and_linewise_changes_repeat_without_selection() {
        let mut character = app();
        character
            .state
            .open_text(crate::document::TextBuffer::from_text("abcdef"), None);
        character.ui.dashboard = false;
        for key in ['v', 'l', 'd', 'l', '.'] {
            character.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = character
            .state
            .active_buffer_id()
            .expect("character buffer");
        assert_eq!(character.state.buffers[buffer].text.to_string(), "cf");

        let mut line = app();
        line.state
            .open_text(crate::document::TextBuffer::from_text("a\nb\nc\nd\n"), None);
        line.ui.dashboard = false;
        for key in ['V', 'j', 'd', '.'] {
            line.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = line.state.active_buffer_id().expect("line buffer");
        assert_eq!(line.state.buffers[buffer].text.to_string(), "");
    }

    #[test]
    fn normalized_blockwise_change_repeats_without_a_stale_selection() {
        let mut app = app();
        app.state.open_text(
            crate::document::TextBuffer::from_text("abc\ndef\nghi\njkl"),
            None,
        );
        app.ui.dashboard = false;
        app.handle_configured_key("ctrl-v".parse().expect("block key"));
        app.handle_configured_key(KeyEvent::character('l'));
        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_key(KeyEvent::character('c'));
        app.handle_configured_key(KeyEvent::character('Z'));
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Escape));
        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_key(KeyEvent::character('.'));
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "Zc\nZf\nZi\nZl");
    }

    #[test]
    fn normalized_block_repeat_pads_new_ragged_and_empty_rows_by_display_column() {
        let mut app = app();
        app.state.open_text(
            crate::document::TextBuffer::from_text("abcd\nx\npqrs\n----\ny\n----"),
            None,
        );
        app.ui.dashboard = false;
        let view = app.state.active_view.expect("view");
        app.state.views[view].cursor = ByteOffset(10);
        app.state.views[view].mode = crate::editing::EditorMode::VisualBlock;
        app.state.views[view].selection = Some(crate::editing::Selection {
            anchor: ByteOffset(2),
            head: ByteOffset(10),
            kind: crate::editing::SelectionKind::Block,
        });
        for key in [
            KeyEvent::character('c'),
            KeyEvent::character('Z'),
            KeyEvent::plain(crate::input::KeyCode::Escape),
        ] {
            app.handle_configured_key(key);
        }
        app.state.views[view].cursor = ByteOffset(14);
        app.state.views[view].selection = None;
        app.handle_configured_key(KeyEvent::character('.'));
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            "abZ\nx Z\npqZ\n--Z\ny Z\n--Z"
        );
    }

    #[test]
    fn replace_mode_session_is_semantically_repeatable() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abc"), None);
        app.ui.dashboard = false;
        app.handle_configured_key(KeyEvent::character('R'));
        app.handle_configured_key(KeyEvent::character('X'));
        app.handle_configured_key(KeyEvent::plain(crate::input::KeyCode::Escape));
        app.handle_configured_key(KeyEvent::character('l'));
        app.handle_configured_key(KeyEvent::character('.'));
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "XXc");
    }

    #[test]
    fn blockwise_visual_commands_record_and_replay_as_typed_macro_invocations() {
        let mut app = app();
        app.state.open_text(
            crate::document::TextBuffer::from_text("abc\ndef\nghi\njkl"),
            None,
        );
        app.ui.dashboard = false;
        app.handle_configured_key(KeyEvent::character('q'));
        app.handle_configured_key(KeyEvent::character('a'));
        app.handle_configured_key("ctrl-v".parse().expect("block key"));
        for key in ['l', 'j', 'd', 'q', 'j', 'j', '@', 'a'] {
            app.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "c\nf\ni\nl");
    }

    #[test]
    fn macro_replay_honors_configured_counts() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abcde"), None);
        app.ui.dashboard = false;
        for key in ['q', 'a', 'x', 'q', '3', '@', 'a'] {
            app.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "e");
    }

    #[test]
    fn macros_record_replay_last_register_and_stop_at_limits() {
        let mut app = app();
        app.state
            .open_text(crate::document::TextBuffer::from_text("abc"), None);
        app.ui.dashboard = false;
        for key in ['q', 'a', 'x', 'q', '@', 'a', '@', '@'] {
            app.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = app.state.active_buffer_id().expect("buffer");
        assert_eq!(app.state.buffers[buffer].text.to_string(), "");
        assert_eq!(app.last_macro, Some('a'));

        let recursive =
            crate::editing::CommandInvocation::new("macro.replay", InvocationSource::Macro)
                .expect("macro command")
                .with_argument("register", CommandValue::String("r".into()));
        app.state.registers.set_macro('r', vec![recursive]);
        app.replay_macro('r');
        assert_eq!(app.replay_depth, 0);
        assert!(app.ui.notification_error);
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|message| message.contains("recursion limit"))
        );
    }
}
