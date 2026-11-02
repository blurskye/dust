//! Original post-authorization document/root/Nix/preflight lineage and launch decisions.
use super::{LspLaunch, LspSession};
use crate::{
    editor::session::workspace::{
        ExecutionLane, ExecutionReady, WorkspaceSession, WorkspaceVersion,
    },
    editor::{EditorState, EditorWindowVersion, text::TextVersion},
    files::ProjectRootResolution,
    language::lsp::{LspDocument, LspSessionKey},
    process::{ExecutionPreflightRequest, PreflightId},
    tools::{NixWorkspaceService, ToolDefinition},
};
use std::{collections::HashMap, path::PathBuf, time::Duration};
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum LspLaunchSource {
    Direct { executable: PathBuf, managed: bool },
    NixWorkspace { nix: PathBuf, shell: String },
}

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct PendingLspProjectRoot {
    pub(in crate::editor::session) asked: bool,
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

pub(in crate::editor::session) struct LspLaunchRequest {
    pub session_key: String,
    pub definition: ToolDefinition,
    pub launch: LspLaunch,
    pub root_id: crate::workspace::RootId,
    pub project_root: PathBuf,
    pub buffer: crate::editor::BufferId,
    pub document: LspDocument,
    pub restart: bool,
}

pub(in crate::editor::session) struct LaunchContext<'a> {
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
    fn capture(
        buffer: crate::editor::BufferId,
        path: &std::path::Path,
        context: &LaunchContext<'_>,
    ) -> Result<Self, String> {
        let document = context
            .editor
            .buffers
            .get(buffer)
            .filter(|document| document.path.as_deref() == Some(path))
            .ok_or("language-server launch document retired")?;
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
    fn current(&self, context: &LaunchContext<'_>) -> bool {
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
                        && self.text.same_incarnation(&buffer.text.source_version())
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
    worker: NixWorkspaceService,
    preflights: HashMap<PreflightId, Preflight>,
}
impl LspSession {
    pub(in crate::editor::session) fn launch_pending(&self, definition: &str) -> bool {
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
    pub(in crate::editor::session) fn project_pending(&self) -> bool {
        self.preparation.root.is_some()
    }
    pub(in crate::editor::session) fn preflight_count(&self) -> usize {
        self.preparation.preflights.len()
    }
    pub(in crate::editor::session) fn owns_preflight(&self, id: PreflightId) -> bool {
        self.preparation.preflights.contains_key(&id)
    }
    pub(in crate::editor::session) fn fail_preflight(&mut self, id: PreflightId) -> bool {
        self.preparation.preflights.remove(&id).is_some()
    }
    pub(in crate::editor::session) fn close_preparation(&mut self) {
        self.retire_launches();
        self.preparation.worker.close();
    }
    pub(in crate::editor::session) fn preparation_pending(&self) -> bool {
        self.preparation.worker.pending()
    }
    pub(in crate::editor::session) fn retire_launches(&mut self) {
        self.preparation.root = None;
        self.preparation.nix = None;
        self.preparation.preflights.clear();
    }
    pub(in crate::editor::session) fn discover_launch(
        &mut self,
        value: PendingLspProjectRoot,
        context: LaunchContext<'_>,
        mut lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        if self.preparation.root.is_some()
            || !lane.current(&context.workspace)
            || !lane.discovery_available()
        {
            return Err("language-server root admission is retired or busy".into());
        }
        let source = Source::capture(value.buffer, &value.document.path, &context)?;
        let generation = lane.discover(
            value.document.path.clone(),
            value.root_id,
            value.definition.project_root_markers.clone(),
            value.definition.project_root_strategy,
        )?;
        self.preparation.root = Some((generation, Pending { value, source }));
        Ok(())
    }
    pub(in crate::editor::session) fn accept_project_root(
        &mut self,
        result: Result<ProjectRootResolution, String>,
        context: LaunchContext<'_>,
        lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        let Some((generation, _)) = &self.preparation.root else {
            return Ok(());
        };
        if result
            .as_ref()
            .is_ok_and(|result| result.generation != *generation)
        {
            return Ok(());
        }
        let (_, pending) = self.preparation.root.take().unwrap();
        let resolution = result?;
        let Pending {
            value: pending,
            source,
        } = pending;
        if !source.current(&context)
            || !lane.current(&source.workspace)
            || resolution.root_revision != lane.root_revision()
            || resolution.document != pending.document.path
            || resolution.workspace_root != pending.workspace_root
        {
            return Err("language-server root source changed".into());
        }
        let Some(project_root) = resolution.project_root else {
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
                let generation = self.preparation.worker.prepare()?;
                self.preparation.nix = Some((
                    generation,
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
        if editor.active_buffer_id() == Some(buffer) {
            self.select(key);
        }
        Ok(())
    }
    pub(in crate::editor::session) fn poll_nix_launch(
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
        let Some((generation, _)) = &self.preparation.nix else {
            return Ok(());
        };
        if result.generation != *generation {
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
    pub(in crate::editor::session) fn queue_launch(
        &mut self,
        value: LspLaunchRequest,
        context: LaunchContext<'_>,
        lane: ExecutionLane<'_>,
    ) -> Result<(), String> {
        if self.preparation.preflights.len() >= 256
            || !lane.current(&context.workspace)
            || !lane.preflight_available()?
        {
            return Err("language-server preflight admission retired or full".into());
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
            return Err("language-server preflight admission retired or full".into());
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
    pub(in crate::editor::session) fn finish_preflight(
        &mut self,
        ready: ExecutionReady,
        editor: &EditorState,
        workspace: &WorkspaceSession,
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
        if workspace.safe_mode() || !settings.enabled || !pending.source.current(&context) {
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
            request_timeout: Duration::from_millis(super::LSP_REQUEST_TIMEOUT_MS),
            maximum_log_lines: super::LSP_MAXIMUM_LOG_LINES,
        };
        let configuration = if settings.inlay_hint_level == crate::config::InlayHintLevel::Max {
            crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(&definition.id)
        } else {
            crate::language::lsp::LspServerConfiguration::for_server(
                &definition.id,
                &result.executable,
            )
        };
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
    pub(in crate::editor::session) fn fixture_preflight_projects(
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
    use super::*;
    #[test]
    fn launch_source_allows_edits_but_rejects_foreign_text_editor_and_configuration() {
        use crate::editor::text::TextBuffer;
        let root = tempfile::tempdir().unwrap();
        let identity = crate::workspace::RootIdentity::capture(root.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let workspace =
            WorkspaceSession::new(state, root.path().join("trust.json"), 1, false, false, 1);
        let mut editor = EditorState::new(root.path().into());
        let path = root.path().join("source.rs");
        let (buffer, _) = editor.open_text(TextBuffer::from_text("old"), Some(path.clone()));
        fn context<'a>(editor: &'a EditorState, workspace: &WorkspaceSession) -> LaunchContext<'a> {
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
        foreign.open_text(editor.buffers[buffer].text.clone(), Some(path.clone()));
        assert!(!source.current(&context(&foreign, &workspace)));
        editor.buffers[buffer].text = TextBuffer::from_text("new");
        assert!(
            !source.current(&context(&editor, &workspace)),
            "equal-byte replacement is not the admitted document"
        );
        editor.buffers.remove(buffer);
        assert!(!source.current(&context(&editor, &workspace)));
    }
}
