mod configuration;
mod contributions;
mod editing;
mod extensions;
mod language;
mod language_features;
pub(super) mod project;
use project::wait_execution_validation;
mod surfaces;
mod viewport;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use crate::editor::input::{InputContextLayer, KeyEvent, MappingResolution};
use crate::editor::text::{ByteOffset, ByteRange, TextBuffer};
use crate::editor::{CommandSource, CommandValue, InvocationSource};
use crate::files::{ExplorerWatchBatch, ExplorerWatchGeneration, ExplorerWatchService};
use crate::ui::{
    ContentTabDiagnostic, ContentTabDiagnosticSeverity, ContentTabKind, DocumentationMarkupKind,
    DocumentationPopupKind, DocumentationPopupState, DocumentationSection, ExplorerSegmentRole,
    ExplorerUiState, PickerSurfaceKind, PtyScreen, TerminalPanelMode, UiEvent, UiFold,
    VisualRowAnchor, VisualRowModel, active_pane_viewport, explorer_panel_width, render,
    snap_display_column,
};
use tempfile::tempdir;

use super::{
    DeferredExecution, InteractiveApplication, KeymapOperation, LSP_INLAY_HINT_WARMUP_RETRY_LIMIT,
    LspProjectionOwner, LspRuntimeSource, PendingExecutionPreflight, PendingVfs, TerminalRuntime,
    build_restored_layout, capture_persisted_layout, compile_keymaps,
    compile_keymaps_with_plugin_catalog, configuration_generation_is_newer, configured_file_bytes,
    dashboard_actions, explorer_watch_policy, lsp_runtime_source_allowed_on_host, make_logs,
    reload_keymaps, resolve_configuration, validate_configured_commands,
};
use crate::editor::{AppEffect, PickerKind, StartupTarget, build_state, register_app_commands};
use crate::{
    extensions::{PluginLoadId, PluginManager},
    language::syntax::SyntaxService,
    persistence::{PersistedWindowPlacement, PersistedWindowRule, PersistenceService},
    process::{ExecutionPreflightService, JobSupervisor, PtyId, PtyService},
    telemetry::CrashSnapshotWriter,
    tools::{PackageService, RegistryService, ToolKind, ToolStatus, is_nixos_host},
    ui::window::{DockEdge, WindowPlacement, WindowRect},
    workspace::{DiagnosticSeverity, WorkspaceTrust},
};

#[test]
fn nix_workspace_fallback_is_reserved_for_nixos_hosts() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, true, true),
        Some("rust-analyzer")
    );
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, true, false),
        None
    );
    assert_eq!(
        super::nix_workspace_shell_for_host(&definition, false, true),
        None
    );
}

#[test]
fn nix_lsp_launch_runs_the_processed_upstream_artifact_without_nix_evaluation() {
    let definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("Rust analyzer catalog definition");
    let launch = super::LspLaunch::NixWorkspace {
        nix: PathBuf::from("/home/user/.local/state/dust/tools/rust-analyzer/bin/rust-analyzer"),
        workspace_root: PathBuf::from("/home/user/.local/state/dust/tools/nix-lsp-workspace"),
        shell: "rust-analyzer".into(),
    };
    assert!(launch.arguments(&definition).is_empty());
    // The synthetic path has no profile, so environment resolution fails
    // closed rather than launching without compatibility.
    assert!(launch.environment(&definition).is_err());
}

fn app() -> InteractiveApplication {
    let root = tempdir().expect("tempdir").keep();
    let startup = StartupTarget::Dashboard { root };
    let state = build_state(&startup);
    let mut commands = crate::editor::CommandRegistry::default();
    crate::editor::register_builtin_commands(&mut commands).expect("editing commands");
    register_app_commands(&mut commands).expect("app commands");
    let loaded = crate::config::load_bundled_config();
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
    let plugin_storage_root = state.workspace_root.join("plugin-storage-test");
    let pty_queue_capacity = configuration.settings.terminals.queue_capacity;
    let terminal_panel_height = configuration.settings.ui.terminal_panel.height;
    let explorer_watch = ExplorerWatchService::start(explorer_watch_policy(
        &configuration.settings.ui.explorer.watch,
    ))
    .unwrap();
    let diagnostics = crate::workspace::DiagnosticStore::new(
        configuration.settings.workspace.maximum_diagnostics,
        configuration
            .settings
            .workspace
            .maximum_diagnostics_per_owner_uri,
    );
    InteractiveApplication {
        state,
        windows: crate::ui::window::WindowGraph::default(),
        editor_windows: crate::editor::session::window_bridge::EditorWindowBridge::default(),
        surface_windows: crate::editor::session::window_bridge::NativeSurfaceBridge::default(),
        plugin_windows: crate::editor::session::extensions::windows::PluginWindowHost::default(),
        pending_plugin_ui_events: BTreeMap::new(),
        plugin_host_events: crate::extensions::PluginHostEventQueue::default(),
        plugin_host_event_observation: None,
        plugin_storage: crate::extensions::PluginStorageService::start(
            plugin_storage_root,
            plugin_queue_capacity,
        ),
        pending_plugin_storage_results: BTreeMap::new(),
        plugin_providers: crate::extensions::PluginProviderBroker::default(),
        plugin_virtual_documents: crate::extensions::PluginVirtualDocumentBroker::default(),
        plugin_virtual_editor:
            crate::editor::session::extensions::documents::VirtualDocumentEditorBridge::default(),
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
        picker_payloads: Default::default(),
        pending_workspace_proposal: None,
        workspace_proposal: None,
        workspace_proposal_response: None,
        workspace_resource_intents: Vec::new(),
        accepted_resource_intents: Vec::new(),
        resource_apply_pending: HashMap::new(),
        next_proposal: 1,
        commands,
        input: crate::editor::input::ModalResolver::default(),
        configuration,
        pending_keymap: None,
        ui: crate::ui::UiState {
            dashboard: true,
            ..Default::default()
        },
        configuration_worker: crate::config::ConfigurationWorker::start(
            false,
            crate::config::ConfigWatchConfig::default(),
            "dust".into(),
        ),
        configuration_generation: 0,
        vfs: crate::files::VfsWorker::start(2),
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
        tools: crate::tools::inspect_tools(),
        lsp_nix_workspace_preferred: BTreeSet::new(),
        lsp_managed_preferred: BTreeSet::new(),
        lsp_managed_versions: BTreeMap::new(),
        tool_installer: crate::tools::ToolInstaller::start(),
        tool_version_service: crate::tools::ToolVersionService::start(),
        tool_version_pending: BTreeSet::new(),
        tool_versions: BTreeMap::new(),
        lsp_last_errors: BTreeMap::new(),
        lsp: crate::language::lsp::LspService::new(),
        active_lsp_server: None,
        lsp_project_roots: crate::files::ProjectRootService::default(),
        pending_lsp_project_root: None,
        nix_lsp_workspace: crate::tools::NixWorkspaceService::default(),
        pending_nix_lsp_workspace: None,
        dap_project_roots: crate::files::ProjectRootService::default(),
        pending_dap_project_root: None,
        lsp_autostart_observation: None,
        lsp_sessions: HashMap::new(),
        lsp_session_synced: HashMap::new(),
        lsp_synced_revisions: Default::default(),
        lsp_capabilities: None,
        lsp_request_generation: 0,
        lsp_inlay_request_generation: 0,
        lsp_inlay_request_owner: None,
        lsp_inlay_refresh_due: None,
        lsp_inlay_empty_retries: 0,
        lsp_logs: Default::default(),
        lsp_progress_epoch: Instant::now(),
        lsp_completion_items: Default::default(),
        snippet_session: None,
        lsp_selection_ranges: Vec::new(),
        lsp_semantic_raw: HashMap::new(),
        lsp_pull_ids: HashMap::new(),
        lsp_projection_owner: None,
        lsp_request_owner: None,
        lsp_completion_owner: None,
        dap: crate::debugger::DapService::new(),
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
        workspace_search: crate::files::WorkspaceSearchService::default(),
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
        workspace_files: crate::files::WorkspaceFileService::default(),
        workspace_file_generation: None,
        workspace_file_root_revision: None,
        workspace_file_entries: Vec::new(),
        workspace_search_generation: None,
        workspace_search_workflow: None,
        workspace_search_sequence: 0,
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
        directory_service: crate::files::DirectoryService::default(),
        explorer: crate::workspace::ExplorerState::default(),
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
        plugin_contributions: crate::extensions::PluginContributionCatalog::default(),
        pending_index_activation: BTreeMap::new(),
        rejected_plugin_loads: BTreeSet::new(),
        registry: RegistryService::start(
            crate::config::PackageRegistryConfig::default(),
            crate::config::user_state_directory().join("packages/test-registry-cache"),
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
    }
}

fn plugin_manifest(owner: &str, mapping_id: &str, keys: &str) -> crate::extensions::PluginManifest {
    crate::extensions::PluginManifest {
        id: owner.into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: BTreeSet::new(),
        commands: [(
            "run".into(),
            crate::extensions::PluginCommandDeclaration {
                title: format!("Run {owner}"),
                description: String::new(),
                arguments: BTreeMap::new(),
                context: crate::extensions::PluginCommandContext::Application,
                activation: crate::extensions::PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: crate::extensions::PluginContributions {
            schema_version: 1,
            input_contexts: vec![crate::extensions::PluginInputContextContribution {
                id: format!("plugin.{owner}.chat"),
                title: format!("{owner} chat"),
                parent_surface: Some("panel".into()),
            }],
            keymaps: vec![crate::extensions::PluginKeymapContribution {
                id: mapping_id.into(),
                context: "dashboard".into(),
                mode: String::new(),
                keys: keys.into(),
                command: format!("plugin.{owner}.run"),
                arguments: BTreeMap::new(),
            }],
            key_groups: Vec::new(),
            content_types: vec![crate::extensions::PluginContentTypeContribution {
                id: format!("plugin.{owner}.panel"),
                title: format!("{owner} panel"),
                presenter: crate::extensions::PluginPresenterKind::RetainedDeclarative,
                default_placement: crate::extensions::PluginWindowPlacementKind::Floating,
                allowed_placements: [
                    crate::extensions::PluginWindowPlacementKind::Floating,
                    crate::extensions::PluginWindowPlacementKind::Hidden,
                ]
                .into_iter()
                .collect(),
                focusable: true,
                maximum_instances: 1,
                context: format!("plugin.{owner}.chat"),
                command: format!("plugin.{owner}.run"),
                private_cells: None,
                provenance: None,
            }],
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        },
    }
}

fn write_plugin_manifest(root: &std::path::Path, manifest: &crate::extensions::PluginManifest) {
    fs::write(
        root.join("manifest.toml"),
        toml::to_string(manifest).expect("manifest TOML"),
    )
    .expect("manifest");
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
