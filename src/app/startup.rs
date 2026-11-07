//! Startup targets, configuration and application construction.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupTarget {
    Dashboard {
        root: PathBuf,
    },
    File {
        workspace_root: PathBuf,
        path: PathBuf,
        additional_files: Vec<PathBuf>,
    },
}

impl StartupTarget {
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        match self {
            Self::Dashboard { root } => root,
            Self::File { workspace_root, .. } => workspace_root,
        }
    }

    /// The files named at startup that do not exist yet.
    #[must_use]
    pub fn new_files(&self) -> Vec<PathBuf> {
        match self {
            Self::Dashboard { .. } => Vec::new(),
            Self::File {
                path,
                additional_files,
                ..
            } => std::iter::once(path)
                .chain(additional_files)
                .filter(|path| !path.exists())
                .cloned()
                .collect(),
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Dashboard { root } => {
                format!("dashboard\t{}", root.to_string_lossy().escape_debug())
            }
            Self::File {
                workspace_root,
                path,
                additional_files,
            } => format!(
                "file\t{}\t{}\t{}",
                workspace_root.to_string_lossy().escape_debug(),
                path.to_string_lossy().escape_debug(),
                additional_files.len()
            ),
        }
    }
}

pub fn resolve_startup(
    paths: &[PathBuf],
    current_directory: &Path,
) -> Result<StartupTarget, StartupError> {
    validate_current_directory(current_directory)?;
    if paths.is_empty() {
        return Ok(StartupTarget::Dashboard {
            root: current_directory.to_path_buf(),
        });
    }

    let first = canonical_or_new(&paths[0], current_directory)?;
    if first.is_dir() {
        if paths.len() != 1 {
            return Err(StartupError::DirectoryWithAdditionalPaths);
        }
        return Ok(StartupTarget::Dashboard { root: first });
    }
    if first.exists() && !first.is_file() {
        return Err(StartupError::UnsupportedPath(first));
    }

    let additional_files = paths[1..]
        .iter()
        .map(|path| {
            let path = canonical_or_new(path, current_directory)?;
            if !path.exists() || path.is_file() {
                Ok(path)
            } else {
                Err(StartupError::UnsupportedPath(path))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StartupTarget::File {
        workspace_root: current_directory.to_path_buf(),
        path: first,
        additional_files,
    })
}

fn validate_current_directory(path: &Path) -> Result<(), StartupError> {
    let metadata = fs::metadata(path).map_err(|source| StartupError::CurrentDirectory {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(StartupError::CurrentDirectoryNotDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

/// Where a new file at `path` lives: its folder resolved when that exists, as an open resolves
/// an existing file.
pub(crate) fn new_file_path(path: &Path) -> PathBuf {
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// An existing path resolved, or a new file in a folder that exists, which the first save creates.
fn canonical_or_new(path: &Path, current_directory: &Path) -> Result<PathBuf, StartupError> {
    let absolute = current_directory.join(path);
    if !absolute.exists() && absolute.parent().is_some_and(Path::is_dir) {
        return Ok(new_file_path(&absolute));
    }
    canonical_existing(&absolute)
}

fn canonical_existing(path: &Path) -> Result<PathBuf, StartupError> {
    path.canonicalize().map_err(|source| StartupError::Path {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("cannot access current directory `{path}`: {source}")]
    CurrentDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("current directory `{0}` is not a directory")]
    CurrentDirectoryNotDirectory(PathBuf),
    #[error("cannot access startup path `{path}`: {source}")]
    Path {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("a directory startup target cannot be combined with additional paths")]
    DirectoryWithAdditionalPaths,
    #[error("startup path `{0}` is neither a regular file nor a directory")]
    UnsupportedPath(PathBuf),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::app::startup::StartupTarget;

    use crate::app::startup::resolve_startup;

    #[test]
    fn no_argument_preserves_exact_cwd_without_parent_discovery() {
        let root = tempdir().expect("tempdir");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested");
        assert_eq!(
            resolve_startup(&[], &nested).expect("startup"),
            StartupTarget::Dashboard { root: nested }
        );
    }

    #[test]
    fn startup_description_escapes_path_controls() {
        let root = std::path::PathBuf::from("/workspace\x1b]0;title\x07");
        let dashboard = StartupTarget::Dashboard { root: root.clone() };
        let file = StartupTarget::File {
            workspace_root: root,
            path: "name\n\t.rs".into(),
            additional_files: Vec::new(),
        };
        assert_eq!(
            dashboard.describe(),
            "dashboard\t/workspace\\u{1b}]0;title\\u{7}"
        );
        assert_eq!(
            file.describe(),
            "file\t/workspace\\u{1b}]0;title\\u{7}\tname\\n\\t.rs\t0"
        );
    }

    #[test]
    fn directory_is_canonicalized_to_dashboard() {
        let root = tempdir().expect("tempdir");
        let target = resolve_startup(&[root.path().join(".")], root.path()).expect("startup");
        assert_eq!(
            target,
            StartupTarget::Dashboard {
                root: root.path().canonicalize().expect("canonical")
            }
        );
    }

    #[test]
    fn relative_existing_paths_use_the_supplied_directory() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.path().join("other.rs"), "// other\n").unwrap();
        let target = resolve_startup(&["main.rs".into(), "other.rs".into()], root.path()).unwrap();
        let canonical = root.path().canonicalize().unwrap();
        assert_eq!(
            target,
            StartupTarget::File {
                workspace_root: root.path().to_path_buf(),
                path: canonical.join("main.rs"),
                additional_files: vec![canonical.join("other.rs")],
            }
        );
        fs::create_dir(root.path().join("nested")).unwrap();
        assert_eq!(
            resolve_startup(&["nested".into()], root.path()).unwrap(),
            StartupTarget::Dashboard {
                root: canonical.join("nested")
            },
        );
    }

    #[test]
    fn a_new_file_in_an_existing_folder_is_a_file_target() {
        let root = tempdir().expect("tempdir");
        let target =
            resolve_startup(&[std::path::PathBuf::from("new.txt")], root.path()).expect("startup");
        assert_eq!(
            target.new_files(),
            vec![root.path().canonicalize().unwrap().join("new.txt")]
        );
        assert!(
            resolve_startup(&[std::path::PathBuf::from("missing/new.txt")], root.path()).is_err(),
            "a folder that does not exist is still an error"
        );
    }

    #[test]
    fn file_uses_current_directory_as_workspace() {
        let root = tempdir().expect("tempdir");
        let file = root.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        let target = resolve_startup(std::slice::from_ref(&file), root.path()).expect("startup");
        assert_eq!(
            target,
            StartupTarget::File {
                workspace_root: root.path().to_path_buf(),
                path: file.canonicalize().expect("canonical"),
                additional_files: Vec::new(),
            }
        );
    }

    #[test]
    fn a_path_in_a_missing_folder_fails_before_application_start() {
        let root = tempdir().expect("tempdir");
        assert!(resolve_startup(&[root.path().join("missing/file")], root.path()).is_err());
    }
}

use crate::editor::EditorState;

use crate::editor::CommandError;

use crate::editor::TransactionError;

use crate::editor::text::EncodingError;
pub(crate) fn build_state(startup: &StartupTarget) -> EditorState {
    EditorState::new(startup.workspace_root().to_path_buf())
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error(transparent)]
    Vfs(#[from] crate::files::VfsError),
    #[error(transparent)]
    Encoding(#[from] EncodingError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Transaction(#[from] TransactionError),
}

use crate::app::App;
use crate::app::InteractiveError;
use crate::app::configuration::Configuration;
use crate::app::configuration::resolve_configuration;
use crate::app::debugger::Debugger;
use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS;
use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS_PER_URI;
use crate::app::documents::Documents;
use crate::app::documents::VFS_QUEUE_CAPACITY;
use crate::app::input::InputState;
use crate::app::persistence::PERSISTENCE_QUEUE_CAPACITY;
use crate::app::persistence::STATE_FILE;
use crate::app::persistence::STATE_MAXIMUM_BYTES;
use crate::app::register_app_commands;
use crate::app::syntax::SYNTAX_QUEUE_CAPACITY;
use crate::app::syntax::Syntax;
use crate::app::windows::Windows;
use crate::app::workspace::TRUST_QUEUE_CAPACITY;
use crate::config::LoadedConfig;
use crate::editor::CommandRegistry;
use crate::editor::input::ABSOLUTE_MAX_COUNT;
use crate::editor::register_builtin_commands;
use crate::editor::text::TextBuffer;
use crate::files::ExplorerWatchWorker;
use crate::files::RootIdentity;
use crate::files::VfsWorker;
use crate::persistence::PersistenceWorker;
use crate::tools::RegistryWorker;
use crate::ui::UiState;
use crate::workspace::WorkspaceId;
use crate::workspace::WorkspaceState;

impl App {
    pub(in crate::app) fn new(
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
        let source_fingerprint = loaded_config.source_fingerprint.clone();
        let config_warning = loaded_config.warning.clone();
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
        state.policy = configuration.settings.editor.editing_policy();
        let persistence = &configuration.settings.persistence;
        state.undo_directory =
            (persistence.enabled && persistence.undo_history).then(|| state_directory.join("undo"));
        state.undo_maximum_bytes = STATE_MAXIMUM_BYTES;
        let dashboard_visible = matches!(startup, StartupTarget::Dashboard { .. })
            && configuration.settings.startup.show_dashboard;
        if matches!(startup, StartupTarget::Dashboard { .. }) && !dashboard_visible {
            state
                .open_text(TextBuffer::from_text(""), None)
                .map_err(|error| InteractiveError::Configuration(error.into()))?;
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
        let diagnostics = crate::app::diagnostics::Diagnostics::new(
            MAXIMUM_DIAGNOSTICS,
            MAXIMUM_DIAGNOSTICS_PER_URI,
        );
        let vfs_queue_capacity = VFS_QUEUE_CAPACITY;
        let syntax_queue_capacity = SYNTAX_QUEUE_CAPACITY;
        let persistence_queue_capacity = PERSISTENCE_QUEUE_CAPACITY;
        let tasks = crate::app::tasks::Tasks::new(&configuration.settings);
        let persistence_path = state_directory.join(STATE_FILE);
        let workspace = crate::app::workspace::Workspace::new(
            workspace,
            state_directory.join("trusted-workspaces.toml"),
            TRUST_QUEUE_CAPACITY,
            initialize_trust,
            safe_mode,
            crate::app::tasks::PROCESS_EVENT_CAPACITY,
        );
        let health = crate::app::health::Health::new(
            persist_logs,
            state_directory.join("live-health.json"),
            clock.monotonic(),
        );
        let plugin_queue_capacity = configuration.settings.plugins.queue_capacity;
        let plugin_storage_root = state_directory.join("plugin-data-v1");
        let registry = RegistryWorker::start(
            crate::tools::RegistryPolicy::default(),
            state_directory.join("packages/registry-cache"),
        )
        .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
        let terminal_session =
            crate::app::terminal::Terminals::new(&configuration.settings.terminals);
        let terminal_panel_height = configuration.settings.ui.terminal_panel.height;
        let explorer_watch =
            ExplorerWatchWorker::start(crate::files::ExplorerWatchPolicy::default())
                .map_err(|error| InteractiveError::Configuration(error.to_string()))?;
        let documents = Documents::new(
            VfsWorker::start(vfs_queue_capacity),
            &startup,
            state.window_version(),
            workspace.root_revision(),
            vfs_queue_capacity,
        );
        let debugger_session = Debugger::new(&configuration.settings.debug, workspace.version());
        let tooling = crate::app::tools::Tools::new(
            tool_inspection
                .join()
                .unwrap_or_else(|_| crate::tools::inspect_tools()),
        );
        let mut application = App {
            state: crate::app::Tracked::new(state),
            extension: crate::app::plugins::Plugins::new(
                plugin_storage_root,
                plugin_queue_capacity,
                state_directory.join("packages"),
                registry,
            ),
            workspace,
            diagnostics,
            workspace_edits: crate::app::workspace::edits::WorkspaceEdits::default(),
            commands,
            effect_authority: crate::editor::CommandAuthority::Host,
            effect_failed: false,
            input: InputState::new(ABSOLUTE_MAX_COUNT),
            input_replacement: crate::app::prompt::replacement::InputReplacement::default(),
            configuration: Configuration::new(
                configuration,
                use_user_configuration,
                source_fingerprint,
            ),
            ui: crate::app::Tracked::new(UiState {
                config_diagnostic,
                ..UiState::default()
            }),
            documents,
            search: crate::app::search::Search::default(),
            should_quit: false,
            last_key_at: None,
            flash: None,
            agent: crate::app::agent::AgentPanel::new(state_directory.join("agents")),
            pull_requests: crate::app::git::pull_requests::PullRequests::default(),
            tutor: crate::app::tutor::Tutor::new(&state_directory),
            palette_recent: Vec::new(),
            border_drag: None,
            dock_drag: None,
            tab_drag: None,
            remote: None,
            switch_ends_terminals: None,
            switch_approved_terminals: Vec::new(),
            syntax_spans_source: (Vec::new(), Vec::new()),
            syntax_fold_positions: Vec::new(),
            format_on_save: std::collections::HashMap::new(),
            pending_saves: std::collections::VecDeque::new(),
            navigation_reads: std::collections::HashMap::new(),
            lsp_asked: None,
            lsp_asked_request_id: None,
            lsp_waiting: None,
            lsp_waiting_server: None,
            lsp_retry_at: None,
            lsp_retries: 0,
            disk_stamps: std::collections::HashMap::new(),
            reload_stamps: std::collections::HashMap::new(),
            next_disk_check: None,
            git_stamp: None,
            replace_query: None,
            substitute_confirm: None,
            session_load_requested: false,
            session_restore_notice: false,
            window_session: Windows::new(dashboard_visible, terminal_panel_height),
            tooling,
            language: crate::app::language::LspSession::default(),
            debugger_session,
            debug_terminal: None,
            recovery: crate::app::documents::recovery::Recovery::new(
                state_directory.join("recovery"),
            ),
            next_transaction: 1_000_000,
            git: crate::app::git::Git::default(),
            clipboard: crate::app::editing::clipboard::Clipboard::default(),
            macro_replays: Vec::new(),
            macro_running: false,
            explorer: crate::app::explorer::Explorer::new(explorer_watch),
            syntax: Syntax::new(syntax_queue_capacity),
            persistence: crate::app::persistence::Persistence::new(
                PersistenceWorker::start(persistence_queue_capacity),
                persistence_path,
            ),
            tasks,
            notices: crate::app::notifications::Notifications::new(),
            health,
            clock,
            terminal_session,
        };
        application.project_configuration();
        application.project_window_activation();
        if let Some(warning) = config_warning.filter(|_| application.ui.config_diagnostic.is_none())
        {
            application.notify_warning(warning);
        }
        Ok(application)
    }
}

impl App {
    pub(in crate::app) fn draw_before_startup_reads(
        &mut self,
        draw: impl FnOnce(&Self) -> Result<bool, crate::ui::TerminalError>,
    ) -> Result<bool, crate::ui::TerminalError> {
        let painted = draw(self)?;
        self.queue_startup_reads();
        Ok(painted)
    }

    pub(in crate::app) fn queue_startup_reads(&mut self) {
        self.documents
            .startup_activation(&self.state, &self.window_session);
        for error in self
            .documents
            .queue_startup(&self.state.window_version(), self.workspace.root_revision())
        {
            self.show_error(error);
        }
        self.finish_startup_reads_if_complete();
    }
    pub(in crate::app) fn open_startup_document(
        &mut self,
        text: TextBuffer,
        path: PathBuf,
    ) -> Result<crate::editor::BufferId, String> {
        self.window_session
            .check_editor_source(&(&*self.state).into())?;
        let mut candidate = self.state.draft();
        let activate = self
            .documents
            .startup_activation(&self.state, &self.window_session);
        let buffer = if activate {
            candidate.open_document(text, path)?.0
        } else {
            candidate.load_document(text, path)?
        };
        self.commit_editor_draft(candidate, None, activate)?;
        if activate {
            self.documents
                .startup_activated(&self.state, &self.window_session);
        }
        Ok(buffer)
    }
    pub(in crate::app) fn finish_startup_reads_if_complete(&mut self) {
        let activate = self
            .documents
            .startup_activation(&self.state, &self.window_session);
        if let Some(buffer) = self
            .documents
            .take_startup_primary(&self.state.window_version(), self.workspace.root_revision())
            && activate
            && self.state.buffers.contains_key(buffer)
        {
            if let Err(error) = self
                .window_session
                .check_editor_source(&(&*self.state).into())
            {
                self.show_error(error);
                return;
            }
            let mut candidate = self.state.draft();
            if candidate.show_document(buffer).is_ok()
                && let Err(error) = self.commit_editor_draft(candidate, None, true)
            {
                self.show_error(error);
            }
        }
    }
}
