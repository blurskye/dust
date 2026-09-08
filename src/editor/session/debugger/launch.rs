//! Original debugger intent and its validation/discovery/preflight/actor lifecycle.
use super::{DebugSession, TerminalPolicy};
use crate::{
    config::{DebugAdapterConfig, DebugConfig, DebugRequestKind, DebugSessionConfig},
    debugger::{DapLaunch, ExecutionScope},
    editor::session::workspace::{
        DebugPermit, ExecutionLane, ExecutionReady, WorkspaceSession, WorkspaceVersion,
    },
    files::ProjectRootResolution,
    process::{ExecutionPreflightRequest, PreflightId},
    tools::{DebugProgramPolicy, ToolDefinition, ToolEntry, ToolKind, ToolStatus},
    workspace::{RootId, RootIdentity},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct DebugIntent {
    nonce: Arc<()>,
}
impl PartialEq for DebugIntent {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.nonce, &other.nonce)
    }
}
impl Eq for DebugIntent {}
impl DebugIntent {
    pub(in crate::editor::session) fn nonce(&self) -> Arc<()> {
        self.nonce.clone()
    }
}
struct Tool {
    definition: ToolDefinition,
    executable: PathBuf,
    document: PathBuf,
    root: RootId,
    workspace_root: PathBuf,
}
enum Phase {
    Validation,
    Discovery,
    Discovering(u64),
    Adapter,
    Program,
    Preflight {
        id: PreflightId,
        request: ExecutionPreflightRequest,
        program: bool,
    },
    Start,
}
pub(super) struct Launch {
    token: DebugIntent,
    name: String,
    configuration: Option<String>,
    revision: u64,
    scope: WorkspaceVersion,
    roots: Vec<RootIdentity>,
    adapter: DebugAdapterConfig,
    session: DebugSessionConfig,
    tool: Option<Tool>,
    cwd: PathBuf,
    program: Option<PathBuf>,
    executable: Option<PathBuf>,
    phase: Phase,
    terminal: Option<TerminalPolicy>,
}
impl DebugSession {
    pub(in crate::editor::session) fn request_debug(
        &mut self,
        name: Option<&str>,
        requested: Option<&str>,
        program: Option<PathBuf>,
        document: Option<(&Path, &str)>,
        policy: &DebugConfig,
        terminals: &crate::config::TerminalsConfig,
        revision: u64,
        workspace: &WorkspaceSession,
        tools: &[ToolEntry],
    ) -> Result<DebugIntent, String> {
        if !self.service.available() {
            return Err("debugger launch is closed or full".into());
        }
        self.refresh(policy, workspace.version());
        let roots = workspace.roots();
        if roots.is_empty() || roots.len() > 64 {
            return Err("debug launch root capacity exceeded".into());
        }
        let (name, configuration, adapter, session, tool, cwd, program) = if let Some(name) = name {
            let session = policy
                .sessions
                .get(name)
                .ok_or("unknown debug configuration")?;
            let adapter = policy
                .adapters
                .get(&session.adapter)
                .ok_or("debug adapter configuration missing")?;
            let root = roots
                .iter()
                .find(|r| Some(r.id) == workspace.active_root())
                .ok_or("debug workspace root missing")?;
            let cwd = session.cwd.as_ref().map_or_else(
                || root.identity.canonical_path.clone(),
                |p| {
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        root.identity.canonical_path.join(p)
                    }
                },
            );
            let program = match session.request {
                DebugRequestKind::Attach => None,
                DebugRequestKind::Launch => Some(
                    session
                        .program
                        .clone()
                        .ok_or("launch debug configuration requires program")?,
                ),
            };
            if adapter.arguments.len() > policy.maximum_arguments
                || adapter.environment.len() > policy.maximum_environment
            {
                return Err("debug configuration exceeds admission bounds".into());
            }
            (
                name.to_owned(),
                Some(name.to_owned()),
                adapter.clone(),
                session.clone(),
                None,
                cwd,
                program,
            )
        } else {
            let (path, language) = document.ok_or("debug launch has no source document")?;
            let entry = if let Some(id) = requested {
                tools
                    .iter()
                    .find(|e| e.definition.id == id && e.definition.kind == ToolKind::Dap)
            } else {
                tools.iter().find(|e| {
                    e.definition.kind == ToolKind::Dap
                        && e.definition.languages.iter().any(|l| l == language)
                        && matches!(e.status, ToolStatus::Installed(_))
                })
            }
            .ok_or("debug adapter is unavailable")?;
            let ToolStatus::Installed(executable) = &entry.status else {
                return Err("debug adapter is not installed".into());
            };
            let program = program
                .or_else(|| {
                    (entry.definition.debug_program_policy == DebugProgramPolicy::Source)
                        .then(|| path.to_path_buf())
                })
                .ok_or("this adapter requires a compiled program")?;
            let root_id = workspace.require_path(path).map_err(|e| e.to_string())?;
            if workspace.require_path(&program).ok() != Some(root_id) {
                return Err("debug program is outside its original root".into());
            }
            let root = roots
                .iter()
                .find(|r| r.id == root_id)
                .ok_or("debug root missing")?;
            let adapter = DebugAdapterConfig {
                program: executable.clone(),
                arguments: entry.definition.launch_args.clone(),
                environment: BTreeMap::new(),
            };
            let session = DebugSessionConfig {
                adapter: entry.definition.id.clone(),
                request: DebugRequestKind::Launch,
                cwd: None,
                program: Some(program.clone()),
                run_in_terminal_profile: None,
                arguments: BTreeMap::from([("stopOnEntry".into(), serde_json::json!(false))]),
                path_maps: BTreeMap::new(),
            };
            let tool = Tool {
                definition: entry.definition.clone(),
                executable: executable.clone(),
                document: path.into(),
                root: root_id,
                workspace_root: root.identity.canonical_path.clone(),
            };
            (
                entry.definition.name.clone(),
                None,
                adapter,
                session,
                Some(tool),
                root.identity.canonical_path.clone(),
                Some(program),
            )
        };
        let terminal = session
            .run_in_terminal_profile
            .as_ref()
            .map(|name| {
                let profile = terminals
                    .profiles
                    .get(name)
                    .ok_or("original debugger terminal profile missing")?;
                if profile.arguments.len() > 4096
                    || profile.environment.len() > 4096
                    || profile.program.as_os_str().len() > 64 * 1024
                    || profile.arguments.iter().map(String::len).sum::<usize>()
                        + profile
                            .environment
                            .iter()
                            .map(|(k, v)| k.len() + v.len())
                            .sum::<usize>()
                        > 1024 * 1024
                {
                    return Err(
                        "original debugger terminal profile exceeds admission bounds".into(),
                    );
                }
                let root = roots
                    .iter()
                    .find(|r| Some(r.id) == workspace.active_root())
                    .ok_or("original debugger terminal root missing")?;
                let cwd = profile.cwd.as_ref().map_or_else(
                    || root.identity.canonical_path.clone(),
                    |p| {
                        if p.is_absolute() {
                            p.clone()
                        } else {
                            root.identity.canonical_path.join(p)
                        }
                    },
                );
                Ok::<_, String>(TerminalPolicy {
                    name: name.clone(),
                    profile: profile.clone(),
                    cwd,
                })
            })
            .transpose()?;
        let token = DebugIntent {
            nonce: Arc::new(()),
        };
        self.launch = Some(Launch {
            token: token.clone(),
            name,
            configuration,
            revision,
            scope: workspace.version(),
            roots: roots.iter().map(|r| r.identity.clone()).collect(),
            adapter,
            session,
            tool,
            cwd,
            program,
            executable: None,
            phase: Phase::Validation,
            terminal,
        });
        Ok(token)
    }
    pub(in crate::editor::session) fn authorize_debug(
        &mut self,
        intent: DebugIntent,
        permit: DebugPermit,
        workspace: &WorkspaceSession,
    ) -> Result<(), String> {
        let launch = self
            .launch
            .as_mut()
            .filter(|l| {
                l.token == intent
                    && l.scope == workspace.version()
                    && matches!(l.phase, Phase::Validation)
            })
            .ok_or("debug launch intent retired or foreign")?;
        if !workspace.consume_debug_permit(permit, &intent.nonce) {
            return Err("debug launch authorization retired or foreign".into());
        }
        launch.phase = if launch.tool.is_some() {
            Phase::Discovery
        } else {
            Phase::Adapter
        };
        Ok(())
    }
    pub(in crate::editor::session) fn owns_debug_preflight(&self, id: PreflightId) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase,Phase::Preflight{id:actual,..} if actual==id))
    }
    pub(in crate::editor::session) fn fail_debug_preflight(&mut self, id: PreflightId) {
        if self.owns_debug_preflight(id) {
            self.launch = None;
        }
    }
    pub(in crate::editor::session) fn debug_root_result(
        &mut self,
        result: Result<ProjectRootResolution, String>,
    ) -> Result<(), String> {
        let Some(launch) = self.launch.as_mut() else {
            return Ok(());
        };
        let Phase::Discovering(generation) = launch.phase else {
            return Ok(());
        };
        let result = result?;
        let tool = launch
            .tool
            .as_ref()
            .ok_or("debug discovery has no original tool")?;
        if result.generation != generation
            || result.document != tool.document
            || result.workspace_root != tool.workspace_root
        {
            return Err("debug discovery source changed".into());
        }
        launch.cwd = result
            .project_root
            .ok_or("debug adapter requires a catalog-backed project root")?;
        launch.phase = Phase::Adapter;
        Ok(())
    }
    pub(in crate::editor::session) fn debug_preflight(
        &mut self,
        ready: ExecutionReady,
    ) -> Result<(), String> {
        let Some(launch) = self.launch.as_mut() else {
            return Err("debug launch closed".into());
        };
        let Phase::Preflight {
            id,
            request,
            program,
        } = &launch.phase
        else {
            return Err("debug preflight is not pending".into());
        };
        if ready.result().id != *id {
            return Err("foreign debug preflight".into());
        }
        let result = ready.into_scoped_result(request, &launch.scope)?;
        if *program {
            launch.program = Some(result.executable);
            launch.phase = Phase::Start;
        } else {
            launch.executable = Some(result.executable);
            launch.cwd = result.cwd;
            if let Some(program) = &mut launch.program {
                if !program.is_absolute() {
                    *program = result.root.join(&*program);
                }
                launch.phase = Phase::Program;
            } else {
                launch.phase = Phase::Start;
            }
        }
        Ok(())
    }
    /// Owner, immutable policy/catalog and actual workspace transport capability stay
    /// borrowed through request construction, real submission and registration.
    pub(in crate::editor::session) fn advance_debug(
        &mut self,
        mut lane: ExecutionLane<'_>,
        policy: &DebugConfig,
        revision: u64,
        tools: &[ToolEntry],
    ) -> Result<Option<String>, String> {
        let Some(mut launch) = self.launch.take() else {
            return Ok(None);
        };
        if !lane.current(&launch.scope)
            || &self.policy != policy
            || launch.revision != revision
            || launch.tool.as_ref().is_some_and(|old| {
                !tools.iter().any(|e| {
                    e.definition == old.definition
                        && matches!(&e.status,ToolStatus::Installed(path) if path==&old.executable)
                })
            })
        {
            return Err("debug launch source retired".into());
        }
        match launch.phase {
            Phase::Validation | Phase::Discovering(_) | Phase::Preflight { .. } => {}
            Phase::Discovery => {
                if !lane.discovery_available() {
                    self.launch = Some(launch);
                    return Ok(None);
                }
                let tool = launch.tool.as_ref().ok_or("debug tool discovery missing")?;
                let generation = lane.discover(
                    tool.document.clone(),
                    tool.root,
                    tool.definition.project_root_markers.clone(),
                    tool.definition.project_root_strategy,
                )?;
                launch.phase = Phase::Discovering(generation);
            }
            Phase::Adapter | Phase::Program => {
                if !lane.preflight_available()? {
                    self.launch = Some(launch);
                    return Ok(None);
                }
                let program = matches!(launch.phase, Phase::Program);
                let request = ExecutionPreflightRequest {
                    executable: if program {
                        launch.program.clone().ok_or("debug program missing")?
                    } else {
                        launch.adapter.program.clone()
                    },
                    cwd: launch.cwd.clone(),
                    roots: launch
                        .roots
                        .iter()
                        .map(|r| r.canonical_path.clone())
                        .collect(),
                    root_revision: lane.root_revision(),
                };
                let id = lane.preflight(request.clone()).map_err(|e| e.to_string())?;
                launch.phase = Phase::Preflight {
                    id,
                    request,
                    program,
                };
            }
            Phase::Start => {
                let admission = self.service.admission().map_err(|e| e.to_string())?;
                let mut paths = vec![launch.cwd.clone()];
                if let Some(program) = &launch.program {
                    paths.push(program.clone());
                }
                let mut arguments = serde_json::Map::from_iter(launch.session.arguments);
                arguments.insert("cwd".into(), serde_json::json!(launch.cwd));
                if let Some(program) = launch.program {
                    arguments.insert("program".into(), serde_json::json!(program));
                }
                admission
                    .submit(
                        DapLaunch {
                            adapter: launch.session.adapter,
                            executable: launch
                                .executable
                                .ok_or("debug adapter was not preflighted")?,
                            arguments: launch.adapter.arguments,
                            environment: launch.adapter.environment.into_iter().collect(),
                            path_maps: launch.session.path_maps,
                            root: launch.cwd,
                            request: match launch.session.request {
                                DebugRequestKind::Launch => "launch",
                                DebugRequestKind::Attach => "attach",
                            }
                            .into(),
                            request_arguments: serde_json::Value::Object(arguments),
                        },
                        Some(ExecutionScope {
                            roots: launch.roots,
                            paths,
                        }),
                    )
                    .map_err(|e| e.to_string())?;
                self.configuration = launch.configuration;
                self.terminal_profile = launch.terminal;
                self.scope = launch.scope;
                self.choices = None;
                self.source = None;
                return Ok(Some(launch.name));
            }
        }
        self.launch = Some(launch);
        Ok(None)
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn debug_program_pending(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase, Phase::Preflight { program: true, .. }))
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn debug_discovering(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase, Phase::Discovering(_)))
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn debug_preflight_paths(&self) -> Option<(&Path, &Path)> {
        let l = self.launch.as_ref()?;
        match &l.phase {
            Phase::Preflight { request, .. } => Some((&request.cwd, l.program.as_deref()?)),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(super) use tests::original_launch_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    pub(in crate::editor::session::debugger) fn original_launch_fixture() {
        use crate::editor::session::workspace::{
            DiscoveryKind, WorkspacePreflightEvent, WorkspaceUpdate,
        };
        use std::time::{Duration, Instant};
        fn permit(workspace: &mut WorkspaceSession, intent: &DebugIntent) -> DebugPermit {
            let id = workspace
                .begin_validation()
                .unwrap_or_else(|_| panic!("owned validation"));
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                for event in workspace.poll() {
                    if let WorkspaceUpdate::Validated {
                        id: actual,
                        permit: Some(permit),
                    } = event
                    {
                        assert_eq!(actual, id);
                        return workspace.debug_permit(permit, id, intent.nonce()).unwrap();
                    }
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(directory.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity.clone())],
        )
        .unwrap();
        let mut workspace =
            WorkspaceSession::new(state, directory.path().join("trust"), 2, false, false, 2);
        workspace
            .fixture_identity(RootId(1), identity.clone())
            .unwrap();
        workspace.fixture_trust_identity(identity);
        workspace.reconcile_trust();
        let mut policy = DebugConfig::default();
        policy.adapters.insert(
            "owned".into(),
            DebugAdapterConfig {
                program: std::env::current_exe().unwrap(),
                arguments: vec!["original".into()],
                environment: BTreeMap::new(),
            },
        );
        policy.sessions.insert(
            "owned".into(),
            DebugSessionConfig {
                adapter: "owned".into(),
                request: DebugRequestKind::Attach,
                cwd: None,
                program: None,
                run_in_terminal_profile: Some("shell".into()),
                arguments: BTreeMap::new(),
                path_maps: BTreeMap::new(),
            },
        );
        let mut owner = DebugSession::new(&policy, workspace.version());
        let terminals = crate::config::TerminalsConfig::default();
        let old = owner
            .request_debug(
                Some("owned"),
                None,
                None,
                None,
                &policy,
                &terminals,
                1,
                &workspace,
                &[],
            )
            .unwrap();
        let old_permit = permit(&mut workspace, &old);
        let fresh = owner
            .request_debug(
                Some("owned"),
                None,
                None,
                None,
                &policy,
                &terminals,
                1,
                &workspace,
                &[],
            )
            .unwrap();
        assert_ne!(old, fresh);
        let captured = owner.launch.as_ref().unwrap().terminal.as_ref().unwrap();
        assert_eq!(captured.name, "shell");
        assert_eq!(captured.cwd, directory.path());
        assert_eq!(captured.profile, terminals.profiles["shell"]);
        assert!(owner.authorize_debug(old, old_permit, &workspace).is_err());
        let fresh_permit = permit(&mut workspace, &fresh);
        owner
            .authorize_debug(fresh.clone(), fresh_permit, &workspace)
            .unwrap();
        owner
            .advance_debug(
                workspace.execution_lane(DiscoveryKind::Debugger),
                &policy,
                1,
                &[],
            )
            .unwrap();
        assert!(matches!(
            owner.launch.as_ref().unwrap().phase,
            Phase::Preflight { .. }
        ));
        assert_eq!(
            owner.launch.as_ref().unwrap().adapter.arguments,
            ["original"]
        );
        // A separately constructed owner cannot consume the first owner's intent.
        let mut foreign = DebugSession::new(&policy, workspace.version());
        let foreign_permit = permit(&mut workspace, &fresh);
        assert!(
            foreign
                .authorize_debug(fresh, foreign_permit, &workspace)
                .is_err()
        );
        let _ = owner.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = workspace.preflight_events();
            if !events.is_empty() {
                for event in events {
                    match event {
                        WorkspacePreflightEvent::Ready(ready) => {
                            assert!(owner.debug_preflight(ready).is_err())
                        }
                        _ => panic!("original preflight failed"),
                    }
                }
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(owner.pending(), 0);
        let intent = owner
            .request_debug(
                Some("owned"),
                None,
                None,
                None,
                &policy,
                &terminals,
                1,
                &workspace,
                &[],
            )
            .unwrap();
        let authorization = permit(&mut workspace, &intent);
        owner
            .authorize_debug(intent, authorization, &workspace)
            .unwrap();
        assert!(
            owner
                .advance_debug(
                    workspace.execution_lane(DiscoveryKind::Debugger),
                    &policy,
                    2,
                    &[]
                )
                .is_err(),
            "configuration revision substitution admitted original launch"
        );
        assert!(owner.launch.is_none());
        owner.begin_shutdown();
        assert!(
            owner
                .request_debug(
                    Some("owned"),
                    None,
                    None,
                    None,
                    &policy,
                    &terminals,
                    2,
                    &workspace,
                    &[]
                )
                .is_err()
        );
    }
}
