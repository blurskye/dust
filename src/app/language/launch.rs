//! Original post-authorization document/root/Nix/preflight lineage and launch decisions.
use crate::app::language::LspLaunch;
use crate::app::language::LspSession;
use crate::app::workspace::ExecutionLane;
use crate::app::workspace::ExecutionReady;
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::editor::EditorState;
use crate::editor::EditorWindowVersion;
use crate::editor::text::TextVersion;
use crate::files::ProjectRootResolution;
use crate::language::lsp::LspDocument;
use crate::language::lsp::LspSessionKey;
use crate::process::ExecutionPreflightRequest;
use crate::process::PreflightId;
use crate::tools::NixWorkspaceWorker;
use crate::tools::ToolDefinition;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
#[derive(Clone, Debug)]
pub(in crate::app) enum LspLaunchSource {
    Direct { executable: PathBuf, managed: bool },
    NixWorkspace { nix: PathBuf, shell: String },
}

#[derive(Clone, Debug)]
pub(in crate::app) struct PendingLspProjectRoot {
    pub(in crate::app) asked: bool,
    pub(in crate::app) definition: ToolDefinition,
    pub(in crate::app) launch_source: LspLaunchSource,
    pub(in crate::app) root_id: crate::workspace::RootId,
    pub(in crate::app) workspace_root: PathBuf,
    pub(in crate::app) buffer: crate::editor::BufferId,
    pub(in crate::app) document: LspDocument,
}

#[derive(Clone, Debug)]
pub(in crate::app) struct PendingNixLspWorkspace {
    pub(in crate::app) session_key: String,
    pub(in crate::app) definition: ToolDefinition,
    pub(in crate::app) nix: PathBuf,
    pub(in crate::app) shell: String,
    pub(in crate::app) root_id: crate::workspace::RootId,
    pub(in crate::app) project_root: PathBuf,
    pub(in crate::app) buffer: crate::editor::BufferId,
    pub(in crate::app) document: LspDocument,
    pub(in crate::app) restart: bool,
}

pub(in crate::app) struct LspLaunchRequest {
    pub session_key: String,
    pub definition: ToolDefinition,
    pub launch: LspLaunch,
    pub root_id: crate::workspace::RootId,
    pub project_root: PathBuf,
    pub buffer: crate::editor::BufferId,
    pub document: LspDocument,
    pub restart: bool,
}

pub(in crate::app) struct LaunchContext<'a> {
    pub editor: &'a EditorState,
    pub workspace: WorkspaceVersion,
    pub configuration: u64,
}
struct Source {
    editor: EditorWindowVersion,
    text: TextVersion,
    buffer: crate::editor::BufferId,
    path: PathBuf,
    workspace: WorkspaceVersion,
    configuration: u64,
}
impl Source {
    pub(in crate::app) fn capture(
        buffer: crate::editor::BufferId,
        path: &std::path::Path,
        context: &LaunchContext<'_>,
    ) -> Result<Self, String> {
        let document = context
            .editor
            .buffers
            .get(buffer)
            .filter(|document| document.path.as_deref() == Some(path))
            .ok_or("language-server launch document is stale")?;
        if context.configuration == u64::MAX || context.editor.window_version().revision().is_none()
        {
            return Err("language-server launch source exhausted".into());
        }
        Ok(Self {
            editor: context.editor.window_version(),
            text: document.text.source_version(),
            buffer,
            path: path.into(),
            workspace: context.workspace.clone(),
            configuration: context.configuration,
        })
    }
    pub(in crate::app) fn current(&self, context: &LaunchContext<'_>) -> bool {
        self.workspace == context.workspace
            && self.configuration == context.configuration
            && self.editor.same_editor(&context.editor.window_version())
            && context.editor.window_version().revision().is_some()
            && context
                .editor
                .buffers
                .get(self.buffer)
                .is_some_and(|buffer| {
                    buffer.path.as_ref() == Some(&self.path)
                        && self.text.same_instance(&buffer.text.source_version())
                })
    }
}
struct Pending<T> {
    value: T,
    source: Source,
}
struct Preflight {
    pending: Pending<LspLaunchRequest>,
    request: ExecutionPreflightRequest,
}
#[derive(Default)]
pub(super) struct Preparation {
    root: Option<(u64, Pending<PendingLspProjectRoot>)>,
    nix: Option<(u64, Pending<PendingNixLspWorkspace>)>,
    worker: NixWorkspaceWorker,
    preflights: HashMap<PreflightId, Preflight>,
    missing_roots: Vec<(String, PathBuf, WorkspaceVersion, u64)>,
}
impl LspSession {
    pub(in crate::app) fn project_root_missing(
        &self,
        server: &str,
        path: &std::path::Path,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> bool {
        self.preparation
            .missing_roots
            .iter()
            .any(|(id, document, source, revision)| {
                id == server
                    && document == path
                    && source == workspace
                    && *revision == configuration
            })
    }
    pub(in crate::app) fn stop_document_launches(&mut self, buffer: crate::editor::BufferId) {
        let mut definitions = Vec::new();
        if let Some((_, pending)) = &self.preparation.root
            && pending.source.buffer == buffer
        {
            definitions.push(pending.value.definition.id.clone());
        }
        if let Some((_, pending)) = &self.preparation.nix
            && pending.source.buffer == buffer
        {
            definitions.push(pending.value.definition.id.clone());
        }
        definitions.extend(
            self.preparation
                .preflights
                .values()
                .filter(|pending| pending.pending.source.buffer == buffer)
                .map(|pending| pending.pending.value.definition.id.clone()),
        );
        for definition in definitions {
            self.cancel_launches(&definition);
            self.mark_stopped(&definition);
        }
    }
    pub(in crate::app) fn cancel_launches(&mut self, definition: &str) {
        if self
            .preparation
            .root
            .as_ref()
            .is_some_and(|(_, p)| p.value.definition.id == definition)
        {
            self.preparation.root = None;
        }
        if self
            .preparation
            .nix
            .as_ref()
            .is_some_and(|(_, p)| p.value.definition.id == definition)
        {
            self.preparation.nix = None;
        }
        self.preparation
            .preflights
            .retain(|_, p| p.pending.value.definition.id != definition);
    }
    pub(in crate::app) fn launch_pending(&self, definition: &str) -> bool {
        self.preparation
            .root
            .as_ref()
            .is_some_and(|(_, p)| p.value.definition.id == definition)
            || self
                .preparation
                .nix
                .as_ref()
                .is_some_and(|(_, p)| p.value.definition.id == definition)
            || self
                .preparation
                .preflights
                .values()
                .any(|p| p.pending.value.definition.id == definition)
    }
    #[cfg(test)]
    pub(in crate::app) fn project_pending(&self) -> bool {
        self.preparation.root.is_some()
    }
    pub(in crate::app) fn preflight_count(&self) -> usize {
        self.preparation.preflights.len()
    }
    pub(in crate::app) fn owns_preflight(&self, id: PreflightId) -> bool {
        self.preparation.preflights.contains_key(&id)
    }
    pub(in crate::app) fn fail_preflight(&mut self, id: PreflightId) -> bool {
        self.preparation.preflights.remove(&id).is_some()
    }
    pub(in crate::app) fn close_preparation(&mut self) {
        self.forget_launches();
        self.preparation.worker.close();
        self.service.begin_shutdown();
    }
    pub(in crate::app) fn preparation_pending(&self) -> bool {
        self.preparation.worker.pending()
    }
    pub(in crate::app) fn forget_server_launches(&mut self, server: &str) {
        if self
            .preparation
            .root
            .as_ref()
            .is_some_and(|(_, pending)| pending.value.definition.id == server)
        {
            self.preparation.root = None;
        }
        if self
            .preparation
            .nix
            .as_ref()
            .is_some_and(|(_, pending)| pending.value.definition.id == server)
        {
            self.preparation.nix = None;
        }
        self.preparation
            .preflights
            .retain(|_, pending| pending.pending.value.definition.id != server);
    }
    pub(in crate::app) fn forget_launches(&mut self) {
        self.preparation.root = None;
        self.preparation.nix = None;
        self.preparation.preflights.clear();
        self.preparation.missing_roots.clear();
    }
    pub(in crate::app) fn discover_launch(
        &mut self,
        value: PendingLspProjectRoot,
        context: LaunchContext<'_>,
        mut lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        if self.preparation.root.is_some()
            || !lane.current(&context.workspace)
            || !lane.discovery_available()
        {
            self.reset_autostart();
            return Err("language-server root request is stale or busy".into());
        }
        let source = Source::capture(value.buffer, &value.document.path, &context)?;
        let request_id = lane.discover(
            value.document.path.clone(),
            value.root_id,
            value.definition.project_root_markers.clone(),
            value.definition.project_root_strategy,
        )?;
        self.preparation.root = Some((request_id, Pending { value, source }));
        Ok(())
    }
    pub(in crate::app) fn accept_project_root(
        &mut self,
        result: Result<ProjectRootResolution, String>,
        context: LaunchContext<'_>,
        lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        let Some((request_id, _)) = &self.preparation.root else {
            return Ok(());
        };
        if result
            .as_ref()
            .is_ok_and(|result| result.request_id != *request_id)
        {
            return Ok(());
        }
        let (_, pending) = self.preparation.root.take().unwrap();
        let resolution = result?;
        let Pending {
            value: pending,
            source,
        } = pending;
        if self.is_stopped(&pending.definition.id)
            || self.explicitly_detached(&pending.definition.id, pending.buffer)
            || !source.current(&context)
            || !lane.current(&source.workspace)
            || resolution.root_revision != lane.root_revision()
            || resolution.document != pending.document.path
            || resolution.workspace_root != pending.workspace_root
        {
            return Err("language-server root source changed".into());
        }
        let Some(project_root) = resolution.project_root else {
            // Keep misses only for this document and configuration. The next candidate
            // can then start instead of repeatedly choosing the server with no root.
            self.preparation
                .missing_roots
                .retain(|(id, path, workspace, configuration)| {
                    id != &pending.definition.id
                        && path == &pending.document.path
                        && workspace == &context.workspace
                        && *configuration == context.configuration
                });
            self.preparation.missing_roots.push((
                pending.definition.id.clone(),
                pending.document.path.clone(),
                context.workspace.clone(),
                context.configuration,
            ));
            self.reset_autostart();
            if pending.asked {
                return Err(format!(
                    "{} starts in a project with {}",
                    pending.definition.name,
                    pending.definition.project_root_markers.join(" or ")
                ));
            }
            return Ok(());
        };
        let session_key = LspSessionKey {
            root_id: pending.root_id.0,
            project_root: project_root.clone(),
            server_id: pending.definition.id.clone(),
        }
        .transport_key();
        if self.running_servers().contains(&session_key) {
            return self.attach_launch_document(
                &session_key,
                pending.buffer,
                pending.document,
                context.editor,
            );
        }
        if self
            .preparation
            .preflights
            .values()
            .any(|p| p.pending.value.session_key == session_key)
        {
            self.reset_autostart();
            return Ok(());
        }
        let restart = self.sessions.contains_key(&session_key);
        match pending.launch_source {
            LspLaunchSource::Direct {
                executable,
                managed,
            } => self.submit_launch(
                Pending {
                    value: LspLaunchRequest {
                        session_key,
                        definition: pending.definition,
                        launch: LspLaunch::Direct {
                            executable,
                            managed,
                        },
                        root_id: pending.root_id,
                        project_root,
                        buffer: pending.buffer,
                        document: pending.document,
                        restart,
                    },
                    source,
                },
                lane,
            ),
            LspLaunchSource::NixWorkspace { nix, shell } => {
                if self.preparation.nix.is_some() {
                    return Err("Nix LSP workspace preparation is already pending".into());
                }
                let request_id = self.preparation.worker.prepare()?;
                self.preparation.nix = Some((
                    request_id,
                    Pending {
                        value: PendingNixLspWorkspace {
                            session_key,
                            definition: pending.definition,
                            nix,
                            shell,
                            root_id: pending.root_id,
                            project_root,
                            buffer: pending.buffer,
                            document: pending.document,
                            restart,
                        },
                        source,
                    },
                ));
                Ok(())
            }
        }
    }
    fn attach_launch_document(
        &mut self,
        key: &str,
        buffer: crate::editor::BufferId,
        mut document: LspDocument,
        editor: &EditorState,
    ) -> Result<(), String> {
        let current = editor
            .buffers
            .get(buffer)
            .ok_or("language-server document closed")?;
        document.version = current.text.revision().0;
        document.text = current.text.to_string();
        match self.sync_owned(key, document, (buffer, current.text.source_version())) {
            // The server went away since it was looked up. Its next start opens this document.
            Err(crate::language::lsp::LspError::NotRunning(_)) => return Ok(()),
            result => result.map_err(|error| error.to_string())?,
        }
        let primary_attached = self
            .active
            .as_ref()
            .and_then(|active| self.sessions.get(active))
            .is_some_and(|active| active.documents.contains_key(&buffer));
        if editor.active_buffer_id() == Some(buffer) && !primary_attached {
            self.select(key);
        }
        Ok(())
    }
    pub(in crate::app) fn poll_nix_launch(
        &mut self,
        context: LaunchContext<'_>,
        lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        let result = match self.preparation.worker.poll_result() {
            Ok(Some(result)) => result,
            Ok(None) => return Ok(()),
            Err(error) => {
                return if self.preparation.nix.take().is_some() {
                    Err(error)
                } else {
                    Ok(())
                };
            }
        };
        let Some((request_id, _)) = &self.preparation.nix else {
            return Ok(());
        };
        if result.request_id != *request_id {
            return Ok(());
        }
        let (
            _,
            Pending {
                value: pending,
                source,
            },
        ) = self.preparation.nix.take().unwrap();
        if !source.current(&context) || !lane.current(&source.workspace) {
            return Err("Nix language-server source changed".into());
        }
        let workspace_root = result.result?;
        if self.running_servers().contains(&pending.session_key) {
            return self.attach_launch_document(
                &pending.session_key,
                pending.buffer,
                pending.document,
                context.editor,
            );
        }
        self.submit_launch(
            Pending {
                value: LspLaunchRequest {
                    session_key: pending.session_key,
                    definition: pending.definition,
                    launch: LspLaunch::NixWorkspace {
                        nix: pending.nix,
                        workspace_root,
                        shell: pending.shell,
                    },
                    root_id: pending.root_id,
                    project_root: pending.project_root,
                    buffer: pending.buffer,
                    document: pending.document,
                    restart: pending.restart,
                },
                source,
            },
            lane,
        )
    }
    pub(in crate::app) fn queue_launch(
        &mut self,
        value: LspLaunchRequest,
        context: LaunchContext<'_>,
        lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        if self.preparation.preflights.len() >= 256
            || !lane.current(&context.workspace)
            || !lane.preflight_available()?
        {
            return Err("language-server preflight request is stale or queue is full".into());
        }
        let source = Source::capture(value.buffer, &value.document.path, &context)?;
        self.submit_launch(Pending { value, source }, lane)
    }
    fn submit_launch(
        &mut self,
        pending: Pending<LspLaunchRequest>,
        mut lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        if self.preparation.preflights.len() >= 256
            || !lane.current(&pending.source.workspace)
            || !lane.preflight_available()?
        {
            return Err("language-server preflight request is stale or queue is full".into());
        }
        let request = lane.request(
            pending.value.launch.executable().clone(),
            pending.value.project_root.clone(),
        );
        let id = lane
            .preflight(request.clone())
            .map_err(|error| error.to_string())?;
        self.preparation
            .preflights
            .insert(id, Preflight { pending, request });
        Ok(())
    }
    pub(in crate::app) fn finish_preflight(
        &mut self,
        ready: ExecutionReady,
        editor: &EditorState,
        workspace: &Workspace,
        configuration: u64,
        settings: &crate::config::LspConfig,
    ) -> Result<Option<String>, String> {
        let Some(Preflight { pending, request }) =
            self.preparation.preflights.remove(&ready.result().id)
        else {
            return Ok(None);
        };
        let context = LaunchContext {
            editor,
            workspace: workspace.version(),
            configuration,
        };
        if self.is_stopped(&pending.value.definition.id)
            || self.explicitly_detached(&pending.value.definition.id, pending.value.buffer)
            || workspace.safe_mode()
            || !settings.enabled
            || !pending.source.current(&context)
        {
            return Err("language-server preflight document source changed".into());
        }
        workspace
            .authorize_execution(crate::workspace::ExecutionSource::ProjectLocal)
            .map_err(|error| error.to_string())?;
        let result = ready.into_execution_result(&request, workspace)?;
        let LspLaunchRequest {
            session_key,
            definition,
            launch,
            root_id,
            project_root,
            buffer,
            mut document,
            restart,
        } = pending.value;
        if !workspace
            .require_path(&document.path)
            .is_ok_and(|id| id == root_id)
            || result.cwd != project_root
            || !document.path.starts_with(&project_root)
            || crate::language::lsp::language_id_for_path(&document.path)
                .is_none_or(|language| language != document.language_id)
            || !definition.supports_language(&document.language_id)
        {
            return Err("language-server preflight project source changed".into());
        }
        let current = &editor.buffers[buffer];
        document.version = current.text.revision().0;
        document.text = current.text.to_string();
        let launch = launch.with_preflighted_executable(result.executable.clone());
        let arguments = launch.arguments(&definition);
        let environment = launch.environment(&definition)?;
        let policy = crate::language::lsp::LspPolicy {
            request_timeout: Duration::from_millis(crate::app::language::LSP_REQUEST_TIMEOUT_MS),
            maximum_log_lines: crate::app::language::LSP_MAXIMUM_LOG_LINES,
        };
        let mut configuration = crate::language::lsp::LspServerConfiguration::for_server(
            &definition.id,
            &result.executable,
        );
        if settings.inlay_hint_level == crate::config::InlayHintLevel::Max {
            configuration.overlay(
                crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&definition.id),
            );
        }
        self.start(
            crate::language::lsp::LspSessionLaunch {
                session_key: &session_key,
                definition: &definition,
                executable: result.executable,
                arguments,
                root: result.cwd,
                document: Some(document),
                policy,
                configuration,
                environment,
            },
            (buffer, current.text.source_version()),
            (root_id, launch, restart),
            editor,
        )?;
        Ok(Some(session_key))
    }
    #[cfg(test)]
    pub(in crate::app) fn fixture_preflight_projects(
        &self,
    ) -> impl Iterator<Item = (&String, &PathBuf)> {
        self.preparation
            .preflights
            .values()
            .map(|p| (&p.pending.value.session_key, &p.pending.value.project_root))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::language::launch::*;
    #[test]
    fn busy_root_discovery_allows_the_same_autostart_observation_to_retry() {
        use crate::app::language::LspAutostartObservation;
        use crate::editor::text::TextBuffer;
        let root = tempfile::tempdir().unwrap();
        let identity = crate::files::RootIdentity::capture(root.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let mut workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
        let identity = workspace.roots()[0].identity.clone();
        workspace
            .fixture_identity(crate::workspace::RootId(1), identity.clone())
            .unwrap();
        workspace.fixture_trust_identity(identity);
        workspace.reconcile_trust();
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source.rs");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "rust-analyzer")
            .unwrap();
        let value = PendingLspProjectRoot {
            asked: false,
            definition,
            launch_source: LspLaunchSource::Direct {
                executable: "/server".into(),
                managed: false,
            },
            root_id: crate::workspace::RootId(1),
            workspace_root: root.path().into(),
            buffer,
            document: LspDocument {
                path: path.clone(),
                language_id: "rust".into(),
                version: 0,
                text: "old".into(),
            },
        };
        let context = LaunchContext {
            editor: &editor,
            workspace: workspace.version(),
            configuration: 1,
        };
        let mut owner = LspSession::default();
        owner.preparation.root = Some((
            1,
            Pending {
                source: Source::capture(buffer, &path, &context).unwrap(),
                value: value.clone(),
            },
        ));
        let observation = LspAutostartObservation {
            buffer,
            path,
            language: "rust".into(),
            root_revision: 1,
            trust: crate::workspace::WorkspaceTrust::Trusted,
            enabled: true,
            auto_start: true,
            nix_workspace_fallback: false,
            launchable_server: Some("another-server".into()),
        };
        assert_eq!(
            owner.observe(observation.clone(), false).as_deref(),
            Some("another-server")
        );
        assert!(
            workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language,)
                .current(&context.workspace)
        );
        assert!(
            owner
                .discover_launch(
                    value,
                    context,
                    workspace.execution_lane(crate::app::workspace::DiscoveryKind::Language,)
                )
                .unwrap_err()
                .contains("busy")
        );
        owner.preparation.root = None;
        assert_eq!(
            owner.observe(observation, false).as_deref(),
            Some("another-server")
        );
    }

    #[test]
    fn attaching_existing_servers_preserves_the_documents_primary() {
        use crate::editor::text::TextBuffer;
        let mut editor = EditorState::new("/fixture".into());
        let path = PathBuf::from("/fixture/source.rs");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        let document = LspDocument {
            path,
            language_id: "rust".into(),
            version: 0,
            text: "old".into(),
        };
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "rust-analyzer")
            .unwrap();
        let mut owner = LspSession::default();
        let mut transports = Vec::new();
        for key in ["primary", "secondary"] {
            owner.fixture_session(
                key.into(),
                crate::app::language::LspRuntimeState {
                    root: crate::workspace::RootId(1),
                    root_path: "/fixture".into(),
                    server_id: key.into(),
                    definition: definition.clone(),
                    launch: crate::app::language::LspLaunch::Direct {
                        executable: "/server".into(),
                        managed: false,
                    },
                    capabilities: None,
                    documents: HashMap::new(),
                    restart_count: 0,
                    restart_due: None,
                    circuit_open: false,
                    desired_running: true,
                    activate_on_restart: false,
                },
            );
            transports.push(owner.fixture_transport(
                key,
                &document,
                (buffer, editor.buffers[buffer].text.source_version()),
            ));
            owner
                .attach_launch_document(key, buffer, document.clone(), &editor)
                .unwrap();
            assert_eq!(owner.active().map(String::as_str), Some("primary"));
        }
        assert!(
            owner.sessions()["secondary"]
                .documents
                .contains_key(&buffer)
        );
        assert!(owner.sessions()["primary"].activate_on_restart);
        assert!(!owner.sessions()["secondary"].activate_on_restart);

        // The first attachment to a different active buffer may become primary.
        let path = PathBuf::from("/fixture/other.rs");
        let (other, _) = editor
            .open_text(TextBuffer::from_text("new"), Some(path.clone()))
            .unwrap();
        owner
            .attach_launch_document(
                "secondary",
                other,
                LspDocument {
                    path,
                    language_id: "rust".into(),
                    version: 0,
                    text: "new".into(),
                },
                &editor,
            )
            .unwrap();
        assert_eq!(owner.active().map(String::as_str), Some("secondary"));
    }

    #[test]
    fn launch_source_allows_edits_but_rejects_foreign_text_editor_and_configuration() {
        use crate::editor::text::TextBuffer;
        let root = tempfile::tempdir().unwrap();
        let identity = crate::files::RootIdentity::capture(root.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let workspace = Workspace::new(state, root.path().join("trust.json"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source.rs");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        fn context<'a>(editor: &'a EditorState, workspace: &Workspace) -> LaunchContext<'a> {
            LaunchContext {
                editor,
                workspace: workspace.version(),
                configuration: 1,
            }
        }
        let source = Source::capture(buffer, &path, &context(&editor, &workspace)).unwrap();
        editor.buffers[buffer].text.restore_text("new").unwrap();
        assert!(
            source.current(&context(&editor, &workspace)),
            "ordinary edits do not restart root discovery"
        );
        assert!(!source.current(&LaunchContext {
            configuration: 2,
            ..context(&editor, &workspace)
        }));
        let mut foreign = EditorState::new(root.path().into());
        foreign
            .open_text(editor.buffers[buffer].text.clone(), Some(path.clone()))
            .unwrap();
        assert!(!source.current(&context(&foreign, &workspace)));
        editor.buffers[buffer].text = TextBuffer::from_text("new");
        assert!(
            !source.current(&context(&editor, &workspace)),
            "equal-byte replacement is not the original document"
        );
        editor.buffers.remove(buffer);
        assert!(!source.current(&context(&editor, &workspace)));
    }
    #[test]
    fn stopping_a_definition_discards_pending_root_nix_and_preflight_launches() {
        use crate::editor::text::TextBuffer;
        let root = tempfile::tempdir().unwrap();
        let identity = crate::files::RootIdentity::capture(root.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source.rs");
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "rust-analyzer")
            .unwrap();
        let document = LspDocument {
            path: path.clone(),
            language_id: "rust".into(),
            version: 0,
            text: "old".into(),
        };
        let context = LaunchContext {
            editor: &editor,
            workspace: workspace.version(),
            configuration: 1,
        };
        let source = || Source::capture(buffer, &path, &context).unwrap();
        let mut owner = LspSession::default();
        owner.preparation.root = Some((
            1,
            Pending {
                source: source(),
                value: PendingLspProjectRoot {
                    asked: true,
                    definition: definition.clone(),
                    launch_source: LspLaunchSource::Direct {
                        executable: "/server".into(),
                        managed: false,
                    },
                    root_id: crate::workspace::RootId(1),
                    workspace_root: root.path().into(),
                    buffer,
                    document: document.clone(),
                },
            },
        ));
        owner.preparation.nix = Some((
            2,
            Pending {
                source: source(),
                value: PendingNixLspWorkspace {
                    session_key: "owned".into(),
                    definition: definition.clone(),
                    nix: "/nix".into(),
                    shell: "default".into(),
                    root_id: crate::workspace::RootId(1),
                    project_root: root.path().into(),
                    buffer,
                    document: document.clone(),
                    restart: false,
                },
            },
        ));
        owner.preparation.preflights.insert(
            PreflightId(3),
            Preflight {
                pending: Pending {
                    source: source(),
                    value: LspLaunchRequest {
                        session_key: "owned".into(),
                        definition: definition.clone(),
                        launch: LspLaunch::Direct {
                            executable: "/server".into(),
                            managed: false,
                        },
                        root_id: crate::workspace::RootId(1),
                        project_root: root.path().into(),
                        buffer,
                        document,
                        restart: false,
                    },
                },
                request: ExecutionPreflightRequest {
                    executable: "/server".into(),
                    cwd: root.path().into(),
                    roots: vec![root.path().into()],
                    root_revision: 1,
                },
            },
        );
        assert!(owner.launch_pending(&definition.id));
        owner.stop_document_launches(buffer);
        assert!(owner.is_stopped(&definition.id));
        assert!(!owner.launch_pending(&definition.id));
        assert!(!owner.owns_preflight(PreflightId(3)));
    }
}
