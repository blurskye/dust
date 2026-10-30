use super::super::configuration::resolve_configuration;
use super::{
    CommandRegistry, ConfigurationSession, DebugSession, DocumentSession, ExplorerWatchService,
    InputSession, InteractiveApplication, InteractiveError, LoadedConfig, PathBuf,
    PersistenceService, RegistryService, RootIdentity, StartupTarget, SyntaxSession, TextBuffer,
    UiState, VfsWorker, WindowSession, WorkspaceId, WorkspaceState, build_state,
    explorer_watch_policy, register_app_commands, register_builtin_commands,
};

impl InteractiveApplication {
    pub(super) fn new(
        startup: StartupTarget,
        loaded_config: LoadedConfig,
        safe_mode: bool,
        state_directory: PathBuf,
        initialize_trust: bool,
        persist_logs: bool,
        clock: std::sync::Arc<dyn crate::clock::Clock>,
    ) -> Result<Self, InteractiveError> {
        // Checking which of the ~300 catalog tools are installed reads every PATH directory;
        // it runs while the configuration below is parsed and compiled.
        let tool_inspection = std::thread::spawn(crate::tools::inspect_tools);
        let mut state = build_state(&startup);
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
            autopairs: configuration.settings.editor.autopairs,
        };
        let persistence = &configuration.settings.persistence;
        state.undo_directory = (persistence.enabled && persistence.undo_history)
            .then(|| state_directory.join("undo"));
        state.undo_maximum_bytes = persistence.maximum_file_bytes;
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
        let diagnostics = super::super::diagnostics::DiagnosticSession::new(
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
        let tasks = crate::editor::session::tasks::TaskSession::new(&configuration.settings);
        let persistence_path = state_directory.join(&configuration.settings.persistence.state_file);
        let workspace = super::super::workspace::WorkspaceSession::new(
            workspace,
            state_directory.join("trusted-workspaces.toml"),
            configuration.settings.workspace.trust_state_queue_capacity,
            initialize_trust,
            safe_mode,
            process_event_capacity,
        );
        let notices = super::super::notifications::NoticeSession::new(
            &configuration.settings,
            persist_logs,
            state_directory.join("live-health.json"),
            clock.monotonic(),
        );
        let plugin_queue_capacity = configuration.settings.plugins.queue_capacity;
        let plugin_storage_root = state_directory.join("plugin-data-v1");
        let registry = RegistryService::start(
            configuration.settings.packages.registry.clone(),
            state_directory.join("packages/registry-cache"),
        )
        .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
        let terminal_session =
            super::super::terminal::TerminalSession::new(&configuration.settings.terminals);
        let terminal_panel_height = configuration.settings.ui.terminal_panel.height;
        let explorer_watch = ExplorerWatchService::start(explorer_watch_policy(
            &configuration.settings.ui.explorer.watch,
        ))
        .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
        let documents = DocumentSession::new(
            VfsWorker::start(vfs_queue_capacity),
            &startup,
            state.window_version(),
            workspace.root_revision(),
            vfs_queue_capacity,
        );
        let debugger_session =
            DebugSession::new(&configuration.settings.debug, workspace.version());
        let tooling = super::super::tooling::ToolingSession::new(
            tool_inspection
                .join()
                .unwrap_or_else(|_| crate::tools::inspect_tools()),
        );
        let mut application = InteractiveApplication {
            state: super::Tracked::new(state),
            extension: super::super::extensions::ExtensionSession::new(
                plugin_storage_root,
                plugin_queue_capacity,
                state_directory.join("packages"),
                registry,
            ),
            workspace,
            diagnostics,
            workspace_edits: super::super::workspace_edits::WorkspaceEditSession::default(),
            commands,
            input: InputSession::new(configuration.settings.input.count_limit),
            configuration: ConfigurationSession::new(configuration, use_user_configuration),
            ui: super::Tracked::new(UiState {
                config_diagnostic,
                ..UiState::default()
            }),
            documents,
            search: super::super::search::SearchSession::default(),
            should_quit: false,
            last_key_at: None,
            flash: None,
            agent: super::agent::AgentPanel::default(),
            pull_requests: super::pull_requests::PullRequests::default(),
            tutor: super::tutor::Tutor::new(&state_directory),
            palette_recent: Vec::new(),
            remote: None,
            syntax_spans_source: (Vec::new(), Vec::new()),
            window_session: WindowSession::new(dashboard_visible, terminal_panel_height),
            tooling,
            language: super::super::lsp::LspSession::default(),
            debugger_session,
            recovery: super::super::recovery::RecoverySession::new(
                state_directory.join("recovery"),
            ),
            next_transaction: 1_000_000,
            git: super::super::git::GitSession::default(),
            clipboard: super::super::clipboard::ClipboardSession::default(),
            explorer: super::super::explorer::ExplorerSession::new(explorer_watch),
            syntax: SyntaxSession::new(syntax_queue_capacity),
            persistence: super::super::persistence::PersistenceSession::new(
                PersistenceService::start(persistence_queue_capacity),
                persistence_path,
            ),
            tasks,
            notices,
            clock,
            terminal_session,
        };
        application.project_configuration();
        application.project_window_activation();
        Ok(application)
    }
}

impl InteractiveApplication {
    pub(super) fn queue_startup_reads(&mut self) {
        for error in self
            .documents
            .queue_startup(&self.state.window_version(), self.workspace.root_revision())
        {
            self.show_error(error);
        }
        self.finish_startup_reads_if_complete();
    }
    pub(super) fn open_startup_document(
        &mut self,
        text: TextBuffer,
        path: PathBuf,
    ) -> Result<crate::editor::BufferId, String> {
        self.window_session
            .check_editor_admission(&(&*self.state).into())?;
        let mut candidate = self.state.draft();
        let (buffer, _) = candidate.open_document(text, path);
        self.commit_editor_draft(candidate, None, true)?;
        Ok(buffer)
    }
    pub(super) fn finish_startup_reads_if_complete(&mut self) {
        if let Some(buffer) = self
            .documents
            .take_startup_primary(&self.state.window_version(), self.workspace.root_revision())
        {
            if self.state.buffers.contains_key(buffer) {
                if let Err(error) = self
                    .window_session
                    .check_editor_admission(&(&*self.state).into())
                {
                    self.show_error(error);
                    return;
                }
                let mut candidate = self.state.draft();
                if candidate.show_document(buffer).is_ok() {
                    if let Err(error) = self.commit_editor_draft(candidate, None, true) {
                        self.show_error(error);
                    }
                }
            }
        }
    }
}
