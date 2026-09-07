use super::*;

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum AdapterContext {
    Plain,
    Formatter {
        buffer: crate::editor::BufferId,
        revision: BufferRevision,
        source: Vec<u8>,
    },
    Linter {
        matcher: crate::config::ProblemMatcherConfig,
    },
    Test {
        matcher: crate::config::ProblemMatcherConfig,
    },
}
pub(in crate::editor::session) enum PendingExecutionPreflight {
    Task {
        requested: String,
        name: String,
        remaining: Vec<String>,
        task: crate::config::TaskConfig,
        adapter: AdapterContext,
    },
    Terminal {
        profile: String,
        config: crate::config::TerminalProfileConfig,
        dap_request: Option<i64>,
    },
    Lsp {
        session_key: String,
        definition: ToolDefinition,
        launch: LspLaunch,
        root_id: crate::workspace::RootId,
        project_root: PathBuf,
        buffer: crate::editor::BufferId,
        document: LspDocument,
        restart: bool,
    },
    DapToolAdapter {
        definition: ToolDefinition,
        /// Canonical catalog-proven project root, distinct from Dust workspace trust scope.
        project_root: PathBuf,
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
        adapter: crate::config::DebugAdapterConfig,
        session: crate::config::DebugSessionConfig,
    },
    DapProgram {
        name: String,
        adapter: crate::config::DebugAdapterConfig,
        session: crate::config::DebugSessionConfig,
        adapter_executable: PathBuf,
        cwd: PathBuf,
        root_revision: u64,
    },
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TaskOutputRecord {
    pub(in crate::editor::session) stream: JobStream,
    pub(in crate::editor::session) text: String,
    pub(in crate::editor::session) truncated: bool,
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct RawLinterDiagnostic {
    pub(in crate::editor::session) line: usize,
    pub(in crate::editor::session) column: usize,
    pub(in crate::editor::session) severity: DiagnosticSeverity,
    pub(in crate::editor::session) message: String,
}
pub(in crate::editor::session) struct PendingLinterBuild {
    pub(in crate::editor::session) owner: DiagnosticOwner,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) waiting: usize,
    pub(in crate::editor::session) failed: bool,
    pub(in crate::editor::session) grouped: BTreeMap<String, Vec<Diagnostic>>,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct MatcherSource {
    pub(in crate::editor::session) path: PathBuf,
    pub(in crate::editor::session) line: usize,
    pub(in crate::editor::session) column: usize,
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TestCaseResult {
    pub(in crate::editor::session) name: String,
    pub(in crate::editor::session) status: String,
    pub(in crate::editor::session) duration: String,
    pub(in crate::editor::session) message: String,
    pub(in crate::editor::session) source: Option<MatcherSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct LspProjectionOwner {
    pub(in crate::editor::session) server: String,
    pub(in crate::editor::session) buffer: crate::editor::BufferId,
    pub(in crate::editor::session) view: ViewId,
    pub(in crate::editor::session) revision: BufferRevision,
    pub(in crate::editor::session) uri: String,
    pub(in crate::editor::session) language: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct LspAutostartObservation {
    pub(in crate::editor::session) buffer: crate::editor::BufferId,
    pub(in crate::editor::session) path: PathBuf,
    pub(in crate::editor::session) language: String,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) trust: WorkspaceTrust,
    pub(in crate::editor::session) enabled: bool,
    pub(in crate::editor::session) auto_start: bool,
    pub(in crate::editor::session) nix_workspace_fallback: bool,
    pub(in crate::editor::session) execution_busy: bool,
    pub(in crate::editor::session) launchable_server: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum LspRuntimeSource {
    Path,
    Managed,
    Nix,
}

impl LspRuntimeSource {
    pub(in crate::editor::session) const fn key(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Managed => "managed",
            Self::Nix => "nix",
        }
    }

    pub(in crate::editor::session) const fn label(self) -> &'static str {
        match self {
            Self::Path => "PATH",
            Self::Managed => "Dust download",
            Self::Nix => "Dust via Nix",
        }
    }
}

pub(in crate::editor::session) const fn lsp_runtime_source_allowed_on_host(
    source: LspRuntimeSource,
    nixos_host: bool,
) -> bool {
    match source {
        LspRuntimeSource::Path => true,
        LspRuntimeSource::Managed => !nixos_host,
        LspRuntimeSource::Nix => nixos_host,
    }
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum LspLaunch {
    Direct {
        executable: PathBuf,
        managed: bool,
    },
    NixWorkspace {
        nix: PathBuf,
        workspace_root: PathBuf,
        shell: String,
    },
}

impl LspLaunch {
    pub(in crate::editor::session) fn executable(&self) -> &PathBuf {
        match self {
            Self::Direct { executable, .. } => executable,
            Self::NixWorkspace { nix, .. } => nix,
        }
    }

    pub(in crate::editor::session) fn arguments(&self, definition: &ToolDefinition) -> Vec<String> {
        match self {
            Self::Direct { .. } => definition.launch_args.clone(),
            Self::NixWorkspace { .. } => definition.launch_args.clone(),
        }
    }

    pub(in crate::editor::session) fn environment(
        &self,
        definition: &ToolDefinition,
    ) -> Result<BTreeMap<String, String>, String> {
        match self {
            Self::Direct { managed, .. } if *managed => Ok(tool_launch_environment(definition)),
            Self::Direct { .. } => Ok(BTreeMap::new()),
            Self::NixWorkspace { workspace_root, .. } => {
                nix_lsp_launch_environment_for_workspace_at(workspace_root, definition)
            }
        }
    }

    pub(in crate::editor::session) fn with_preflighted_executable(
        self,
        executable: PathBuf,
    ) -> Self {
        match self {
            Self::Direct { managed, .. } => Self::Direct {
                executable,
                managed,
            },
            Self::NixWorkspace {
                workspace_root,
                shell,
                ..
            } => Self::NixWorkspace {
                nix: executable,
                workspace_root,
                shell,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum LspLaunchSource {
    Direct { executable: PathBuf, managed: bool },
    NixWorkspace { nix: PathBuf, shell: String },
}

pub(in crate::editor::session) fn nix_workspace_shell_for_host(
    definition: &ToolDefinition,
    fallback_enabled: bool,
    nixos_host: bool,
) -> Option<&str> {
    fallback_enabled
        .then_some(())
        .filter(|()| nixos_host)
        .and_then(|()| nix_lsp_shell(definition))
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct LspRuntimeState {
    pub(in crate::editor::session) root: crate::workspace::RootId,
    /// Canonical project root accepted by the selected tool policy.
    pub(in crate::editor::session) root_path: PathBuf,
    pub(in crate::editor::session) server_id: String,
    pub(in crate::editor::session) definition: ToolDefinition,
    pub(in crate::editor::session) launch: LspLaunch,
    pub(in crate::editor::session) capabilities: Option<LspCapabilities>,
    pub(in crate::editor::session) documents: HashMap<crate::editor::BufferId, LspDocument>,
    pub(in crate::editor::session) restart_count: u8,
    pub(in crate::editor::session) restart_due: Option<Instant>,
    pub(in crate::editor::session) circuit_open: bool,
    pub(in crate::editor::session) desired_running: bool,
    pub(in crate::editor::session) activate_on_restart: bool,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingLspProjectRoot {
    pub(in crate::editor::session) definition: ToolDefinition,
    pub(in crate::editor::session) launch_source: LspLaunchSource,
    pub(in crate::editor::session) root_id: crate::workspace::RootId,
    pub(in crate::editor::session) workspace_root: PathBuf,
    pub(in crate::editor::session) buffer: crate::editor::BufferId,
    pub(in crate::editor::session) document: LspDocument,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingNixLspWorkspace {
    pub(in crate::editor::session) session_key: String,
    pub(in crate::editor::session) definition: ToolDefinition,
    pub(in crate::editor::session) nix: PathBuf,
    pub(in crate::editor::session) shell: String,
    pub(in crate::editor::session) root_id: crate::workspace::RootId,
    pub(in crate::editor::session) project_root: PathBuf,
    pub(in crate::editor::session) buffer: crate::editor::BufferId,
    pub(in crate::editor::session) document: LspDocument,
    pub(in crate::editor::session) restart: bool,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingDapProjectRoot {
    pub(in crate::editor::session) definition: ToolDefinition,
    pub(in crate::editor::session) adapter_executable: PathBuf,
    pub(in crate::editor::session) root_id: crate::workspace::RootId,
    pub(in crate::editor::session) workspace_root: PathBuf,
    pub(in crate::editor::session) document: LspDocument,
    pub(in crate::editor::session) program: PathBuf,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct SyntaxFailure {
    pub(in crate::editor::session) revision: BufferRevision,
    pub(in crate::editor::session) generation: u64,
    pub(in crate::editor::session) language: SyntaxLanguage,
    pub(in crate::editor::session) diagnostic: String,
}
pub(in crate::editor::session) struct TerminalRuntime {
    pub(in crate::editor::session) profile: String,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) screen: PtyScreen,
    pub(in crate::editor::session) running: bool,
    pub(in crate::editor::session) exit_code: Option<u32>,
    pub(in crate::editor::session) truncated: bool,
    pub(in crate::editor::session) mode: TerminalPanelMode,
    pub(in crate::editor::session) normal_cursor: Option<(usize, usize)>,
    pub(in crate::editor::session) visual_anchor: Option<(usize, usize)>,
    pub(in crate::editor::session) visual_linewise: bool,
    pub(in crate::editor::session) scroll_offset: usize,
}

pub(in crate::editor::session) fn terminal_row_end(row: &[crate::ui::PtyCell]) -> usize {
    row.iter()
        .rposition(|cell| !cell.continuation && cell.text != " ")
        .unwrap_or(0)
}

pub(in crate::editor::session) fn clamp_terminal_cursor(
    screen: &PtyScreen,
    cursor: (usize, usize),
) -> (usize, usize) {
    let row = cursor.0.min(screen.history_len().saturating_sub(1));
    let column = cursor.1.min(
        screen
            .history_row(row)
            .map_or(0, |cells| cells.len().saturating_sub(1)),
    );
    (row, column)
}

pub(in crate::editor::session) fn terminal_yank_text(
    screen: &PtyScreen,
    first: (usize, usize),
    second: (usize, usize),
    linewise: bool,
) -> String {
    if screen.history_len() == 0 {
        return String::new();
    }
    let (start, end) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let mut output = String::new();
    for row_index in start.0..=end.0.min(screen.history_len().saturating_sub(1)) {
        let Some(row) = screen.history_row(row_index) else {
            continue;
        };
        let first_column = if linewise || row_index > start.0 {
            0
        } else {
            start.1
        };
        let last_column = if linewise || row_index < end.0 {
            terminal_row_end(row)
        } else {
            end.1.min(row.len().saturating_sub(1))
        };
        if first_column <= last_column {
            for cell in row
                .iter()
                .take(last_column.saturating_add(1))
                .skip(first_column)
            {
                if !cell.continuation {
                    output.push_str(&cell.text);
                }
            }
        }
        if row_index < end.0 || linewise {
            output.push('\n');
        }
    }
    output
}

pub(in crate::editor::session) fn keep_terminal_cursor_visible(terminal: &mut TerminalRuntime) {
    let total = terminal.screen.scrollback_len() + terminal.screen.dimensions().1;
    let visible = terminal.screen.dimensions().1.max(1);
    let Some((row, _)) = terminal.normal_cursor else {
        terminal.scroll_offset = total.saturating_sub(visible);
        return;
    };
    if row < terminal.scroll_offset {
        terminal.scroll_offset = row;
    } else if row >= terminal.scroll_offset.saturating_add(visible) {
        terminal.scroll_offset = row.saturating_add(1).saturating_sub(visible);
    }
    terminal.scroll_offset = terminal.scroll_offset.min(total.saturating_sub(visible));
}

pub(in crate::editor::session) struct RunningTask {
    pub(in crate::editor::session) requested: String,
    pub(in crate::editor::session) name: String,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) remaining: Vec<String>,
    pub(in crate::editor::session) adapter: AdapterContext,
    pub(in crate::editor::session) root: PathBuf,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingKeymap {
    pub(in crate::editor::session) context: String,
    pub(in crate::editor::session) prefix: String,
    pub(in crate::editor::session) started_at: Instant,
    pub(in crate::editor::session) deadline: Option<Instant>,
    pub(in crate::editor::session) configuration_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) enum LargeFileOverride {
    Auto,
    On,
    Off,
}
pub(in crate::editor::session) fn explorer_watch_policy(
    config: &crate::config::ExplorerWatchConfig,
) -> ExplorerWatchPolicy {
    ExplorerWatchPolicy {
        enabled: config.enabled,
        poll_interval: Duration::from_millis(config.poll_interval_ms),
        debounce: Duration::from_millis(config.debounce_ms),
        maximum_directories: config.maximum_directories,
        maximum_entries_per_directory: config.maximum_entries_per_directory,
        maximum_events: config.maximum_events,
        maximum_path_bytes: config.maximum_path_bytes,
        queue_capacity: config.queue_capacity,
    }
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingSessionRestore {
    pub(in crate::editor::session) session: PersistedSession,
    pub(in crate::editor::session) waiting: usize,
    pub(in crate::editor::session) loaded: HashMap<PathBuf, (PathBuf, TextBuffer)>,
    pub(in crate::editor::session) failed: bool,
}

#[derive(Clone)]
pub(in crate::editor::session) struct SnippetSession {
    pub(in crate::editor::session) buffer: crate::editor::BufferId,
    pub(in crate::editor::session) revision: BufferRevision,
    pub(in crate::editor::session) groups: Vec<Vec<ByteRange>>,
    pub(in crate::editor::session) current: usize,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingWorkspaceProposal {
    pub(in crate::editor::session) id: crate::workspace::ProposalId,
    pub(in crate::editor::session) encoding: PositionEncoding,
    pub(in crate::editor::session) label: String,
    pub(in crate::editor::session) documents: Vec<crate::language::lsp::LspDocumentEdit>,
    pub(in crate::editor::session) snapshots: Vec<crate::workspace::FileSnapshot>,
    pub(in crate::editor::session) creates: Vec<PathBuf>,
    pub(in crate::editor::session) resource_intents: Vec<CheckedResourceIntent>,
    pub(in crate::editor::session) resolved_paths: HashMap<PathBuf, PathBuf>,
    pub(in crate::editor::session) waiting: usize,
    pub(in crate::editor::session) response:
        Option<(String, crate::language::lsp::ServerRequestId)>,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum DeferredExecution {
    InstallTool {
        id: String,
        source: Option<LspRuntimeSource>,
    },
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
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunLinter {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunFormatter {
        name: String,
        buffer: crate::editor::BufferId,
        revision: BufferRevision,
        source: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingRootOperation {
    pub(in crate::editor::session) generation: u64,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) requested_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingExecutionValidation {
    pub(in crate::editor::session) action: DeferredExecution,
    pub(in crate::editor::session) root_revision: u64,
    pub(in crate::editor::session) failed: bool,
}

pub(in crate::editor::session) enum WorkspaceSearchIntent {
    Find,
    Replace(String),
}

pub(in crate::editor::session) fn compose_workspace_file_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let mut broker = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Files,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    })?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: items
            .iter()
            .map(|(item, _)| picker_item_adapter(item))
            .collect(),
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut map = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect())
}

pub(in crate::editor::session) fn compose_plugin_picker_items(
    projection: &crate::extensions::PluginPickerProjection,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(projection.request),
        source: SearchSourceKind::PluginProvider {
            provider: projection.provider.clone(),
        },
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: projection.items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(
        SearchRequestId(projection.request),
        SearchStreamGeneration(projection.revision),
        &projection.query,
    )?;
    let workflow = projection
        .items
        .iter()
        .map(|item| plugin_item_adapter(&item.id, &item.primary, item.secondary.as_deref()))
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(projection.request),
        generation: SearchStreamGeneration(projection.revision),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let snapshot = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?;
    let mut source = projection
        .items
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(snapshot
        .items
        .into_iter()
        .filter_map(|ranked| {
            source.remove(&ranked.stable_key).map(|item| {
                let id = PickerItemId(format!(
                    "plugin-provider:{}:{}:{}",
                    projection.request, projection.revision, item.id
                ));
                (
                    PickerItem {
                        id,
                        label: item.primary.clone(),
                        detail: item.secondary.clone().unwrap_or_default(),
                        source: projection.provider.clone(),
                        action: PickerAction::None,
                    },
                    PickerPayload::PluginProviderItem {
                        plugin: projection.plugin.clone(),
                        load: projection.load,
                        request: projection.request,
                        item: item.id.clone(),
                        action_command: projection.action_command.clone(),
                    },
                )
            })
        })
        .collect())
}

pub(in crate::editor::session) fn compose_command_picker_items(
    items: Vec<(PickerItem, PickerPayload)>,
) -> Result<Vec<(PickerItem, PickerPayload)>, crate::workspace::search_workflow::SearchWorkflowError>
{
    use crate::workspace::search_workflow::*;
    let spec = SearchWorkflowSpec {
        id: SearchWorkflowId(1),
        source: SearchSourceKind::Commands,
        matcher: SearchMatcherKind::Fuzzy,
        presenter: SearchPresenterKind::Picker,
        maximum_items: items.len().max(1),
        maximum_bytes: ABS_MAX_BYTES,
    };
    let mut broker = SearchWorkflowBroker::try_new(spec)?;
    broker.begin(SearchRequestId(1), SearchStreamGeneration(1), "")?;
    let workflow = items
        .iter()
        .map(|(item, payload)| {
            let mut item = item.clone();
            if let PickerPayload::Command(command) = payload {
                item.action = PickerAction::InvokeCommand {
                    command: command.clone(),
                }
            }
            picker_item_adapter(&item)
        })
        .collect();
    broker.accept(SearchWorkflowChunk {
        request: SearchRequestId(1),
        generation: SearchStreamGeneration(1),
        sequence: 0,
        items: workflow,
        complete: true,
    })?;
    let order = broker
        .snapshot_if_changed(0)
        .ok_or(SearchWorkflowError::Stale)?
        .items
        .into_iter()
        .map(|item| item.stable_key)
        .collect::<Vec<_>>();
    let mut by_id = items
        .into_iter()
        .map(|pair| (pair.0.id.0.clone(), pair))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect())
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum PickerPayload {
    WorkspaceSearch(WorkspaceSearchMatch),
    LspLocation(crate::language::lsp::LspLocation),
    Diagnostic {
        uri: String,
        range: ByteRange,
    },
    CodeAction(crate::language::lsp::LspCodeAction),
    Buffer(crate::editor::BufferId),
    Command(String),
    ProposalFile(usize),
    WorkspaceRoot(crate::workspace::RootId),
    Terminal(crate::process::PtyId),
    LspSession(String),
    DapControl(DapControl),
    PersistedLocation(PersistedLocation),
    GitHunk {
        path: PathBuf,
        status_revision: u64,
        repository: crate::workspace::git::GitRepositoryId,
        hunk: crate::workspace::git::GitHunkId,
        new_line: usize,
    },
    MatcherSource(MatcherSource),
    DapFrame(DapStackFrame),
    RegistrySummary(RegistryPackageSummary),
    RegistryMetadata(Box<RegistryPackageMetadata>),
    PluginProviderItem {
        plugin: String,
        load: PluginLoadId,
        request: u64,
        item: String,
        action_command: String,
    },
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum PendingVfs {
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
        range: crate::language::lsp::LspRange,
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
    PickerPreview {
        path: PathBuf,
        generation: u64,
    },
    ExplorerMutation,
    StartupOpen {
        primary: bool,
    },
    Reload {
        buffer: crate::editor::BufferId,
    },
    Save {
        buffer: crate::editor::BufferId,
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
        buffer: crate::editor::BufferId,
        revision: BufferRevision,
    },
    CrashDelete,
    RecoveryDelete {
        path: PathBuf,
        buffer: Option<(crate::editor::BufferId, BufferRevision)>,
    },
}

pub(in crate::editor::session) fn plugin_owner_from_id(id: &str) -> Option<&str> {
    let mut segments = id.split('.');
    (segments.next()? == "plugin")
        .then(|| segments.next())
        .flatten()
        .filter(|owner| !owner.is_empty())
}
