//! Original debugger intent and its discovery/preflight/actor lifecycle.
use crate::app::debugger::Debugger;
use crate::app::debugger::TerminalPolicy;
use crate::app::workspace::ExecutionLane;
use crate::app::workspace::ExecutionReady;
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::config::DebugAdapterConfig;
use crate::config::DebugConfig;
use crate::config::DebugRequestKind;
use crate::config::DebugSessionConfig;
use crate::debugger::DapLaunch;
use crate::debugger::ExecutionScope;
use crate::files::ProjectRootResolution;
use crate::files::RootIdentity;
use crate::process::ExecutionPreflightRequest;
use crate::process::PreflightId;
use crate::tools::DebugProgramPolicy;
use crate::tools::ToolDefinition;
use crate::tools::ToolEntry;
use crate::tools::ToolKind;
use crate::tools::ToolStatus;
use crate::workspace::RootId;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub(in crate::app) struct DebugIntent {
    nonce: Arc<()>,
}
impl PartialEq for DebugIntent {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.nonce, &other.nonce)
    }
}
impl Eq for DebugIntent {}
struct Tool {
    definition: ToolDefinition,
    executable: PathBuf,
    document: PathBuf,
    root: RootId,
    workspace_root: PathBuf,
}
enum Phase {
    Requested,
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
    root_selected: bool,
    program: Option<PathBuf>,
    /// The adapter runs the program itself, so a script needs no execute bit.
    source_program: bool,
    automatic_program: bool,
    executable: Option<PathBuf>,
    phase: Phase,
    terminal: Option<TerminalPolicy>,
}
/// The target an ATTACH names, taken from the configuration's own adapter arguments. A launch
/// returns nothing here: a program is not a target that was attached to.
fn declared_target(arguments: &BTreeMap<String, serde_json::Value>) -> Option<String> {
    ["processId", "pid", "name"].into_iter().find_map(|key| {
        arguments.get(key).and_then(|value| match value {
            serde_json::Value::Number(number) => Some(format!("{key} {number}")),
            serde_json::Value::String(text) => Some(format!("{key} {text}")),
            _ => None,
        })
    })
}
impl Launch {
    /// The identity of what this session will be attached to or running, taken from the
    /// CONFIGURATION rather than invented.
    fn identity(&self) -> crate::app::debugger::DapTargetIdentity {
        crate::app::debugger::DapTargetIdentity {
            configuration: self.configuration.clone(),
            adapter: self.session.adapter.clone(),
            request: self.session.request.clone(),
            program: self.program.clone(),
            automatic_program: self.automatic_program,
            document: self.tool.as_ref().map(|tool| tool.document.clone()),
            target: match self.session.request {
                DebugRequestKind::Attach => declared_target(&self.session.arguments),
                DebugRequestKind::Launch => None,
            },
        }
    }
}
impl Debugger {
    pub(in crate::app) fn request_debug(
        &mut self,
        selection: (Option<&str>, Option<&str>),
        program: Option<PathBuf>,
        document: Option<(&Path, &str)>,
        policy: (&DebugConfig, &crate::config::TerminalsConfig, u64),
        workspace: &Workspace,
        tools: &[ToolEntry],
    ) -> Result<DebugIntent, String> {
        self.request_debug_inner(
            selection, program, document, policy, workspace, tools, false,
        )
    }
    pub(in crate::app) fn request_restart(
        &mut self,
        policy: (&DebugConfig, &crate::config::TerminalsConfig, u64),
        workspace: &Workspace,
        tools: &[ToolEntry],
    ) -> Result<DebugIntent, String> {
        let target = self.target.clone().ok_or("debugger has no launch target")?;
        let document = target.document.as_deref().and_then(|path| {
            crate::language::lsp::language_id_for_path(path).map(|language| (path, language))
        });
        let intent = self.request_debug_inner(
            (target.configuration.as_deref(), Some(&target.adapter)),
            target.program.clone().filter(|_| !target.automatic_program),
            document,
            policy,
            workspace,
            tools,
            true,
        )?;
        let launch = self.launch.take();
        match self.stop() {
            Ok(()) | Err(crate::debugger::DapError::NotRunning) => {}
            Err(error) => return Err(error.to_string()),
        }
        self.launch = launch;
        Ok(intent)
    }
    #[allow(clippy::too_many_arguments)]
    fn request_debug_inner(
        &mut self,
        selection: (Option<&str>, Option<&str>),
        program: Option<PathBuf>,
        document: Option<(&Path, &str)>,
        policy: (&DebugConfig, &crate::config::TerminalsConfig, u64),
        workspace: &Workspace,
        tools: &[ToolEntry],
        restarting: bool,
    ) -> Result<DebugIntent, String> {
        let (name, requested) = selection;
        let automatic_program = name.is_none() && program.is_none();
        let (policy, terminals, revision) = policy;
        if !(if restarting {
            self.service.restart_available()
        } else {
            self.service.available()
        }) {
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
            if adapter.arguments.len() > crate::app::debugger::MAXIMUM_DEBUG_ARGUMENTS
                || adapter.environment.len() > crate::app::debugger::MAXIMUM_DEBUG_ENVIRONMENT
            {
                return Err("debug configuration exceeds its size limits".into());
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
            .ok_or_else(|| {
                let known = tools
                    .iter()
                    .filter(|e| {
                        e.definition.kind == ToolKind::Dap
                            && e.definition.languages.iter().any(|l| l == language)
                    })
                    .map(|e| e.definition.name.as_str())
                    .collect::<Vec<_>>();
                if known.is_empty() {
                    format!("no debugger is known for {language}")
                } else {
                    format!(
                        "no debugger installed for {language}: install {} with <leader>cm",
                        known.join(" or ")
                    )
                }
            })?;
            let ToolStatus::Installed(executable) = &entry.status else {
                return Err("debug adapter is not installed".into());
            };
            let program = program
                .or_else(|| {
                    (entry.definition.debug_program_policy == DebugProgramPolicy::Source)
                        .then(|| path.to_path_buf())
                })
                .ok_or("build the program, then run :dap.session.start program=<path to it>")?;
            let root_id = workspace.require_path(path).map_err(|e| e.to_string())?;
            let root = roots
                .iter()
                .find(|r| r.id == root_id)
                .ok_or("debug root missing")?;
            let program = if program.is_absolute() {
                program
            } else {
                root.identity.canonical_path.join(program)
            };
            if workspace.require_path(&program).ok() != Some(root_id) {
                return Err("debug program is outside its original root".into());
            }
            let adapter = DebugAdapterConfig {
                program: executable.clone(),
                arguments: entry.definition.launch_args.clone(),
                // The program being debugged runs with what the user's shell gave Dust. Delve
                // also needs it to find `go`, and hangs without one.
                environment: std::env::vars_os()
                    .filter_map(|(key, value)| {
                        Some((key.into_string().ok()?, value.into_string().ok()?))
                    })
                    .collect(),
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
        // A configured adapter owns how its target is run; its arbitrary ID says nothing
        // about whether that target needs an execute bit.
        let source_program = configuration.is_some()
            || tools.iter().any(|e| {
                e.definition.id == session.adapter
                    && e.definition.debug_program_policy == DebugProgramPolicy::Source
            });
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
                    return Err("original debugger terminal profile exceeds its size limits".into());
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
            root_selected: false,
            program,
            source_program,
            automatic_program,
            executable: None,
            phase: Phase::Requested,
            terminal,
        });
        Ok(token)
    }
    pub(super) fn apply_preset(
        &mut self,
        root: &Path,
        arguments: BTreeMap<String, serde_json::Value>,
        module: bool,
    ) {
        if let Some(launch) = self.launch.as_mut() {
            launch.cwd = root.to_path_buf();
            launch.root_selected = true;
            launch.session.arguments = arguments;
            if module {
                launch.program = None;
            }
        }
    }
    pub(in crate::app) fn authorize_debug(
        &mut self,
        intent: DebugIntent,
        workspace: &Workspace,
    ) -> Result<(), String> {
        let launch = self
            .launch
            .as_mut()
            .filter(|l| {
                l.token == intent
                    && l.scope == workspace.version()
                    && matches!(l.phase, Phase::Requested)
            })
            .ok_or("debug launch intent is stale or foreign")?;
        workspace
            .execution_allowed()
            .map_err(|_| "debuggers run only in a trusted workspace".to_owned())?;
        launch.phase = if launch.tool.is_some() && !launch.root_selected {
            Phase::Discovery
        } else {
            Phase::Adapter
        };
        Ok(())
    }
    pub(in crate::app) fn owns_debug_preflight(&self, id: PreflightId) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase,Phase::Preflight{id:actual,..} if actual==id))
    }
    pub(in crate::app) fn fail_debug_preflight(&mut self, id: PreflightId) {
        if self.owns_debug_preflight(id) {
            self.launch = None;
        }
    }
    pub(in crate::app) fn debug_root_result(
        &mut self,
        result: Result<ProjectRootResolution, String>,
    ) -> Result<(), String> {
        let Some(launch) = self.launch.as_mut() else {
            return Ok(());
        };
        let Phase::Discovering(request_id) = launch.phase else {
            return Ok(());
        };
        let result = result?;
        let tool = launch
            .tool
            .as_ref()
            .ok_or("debug discovery has no original tool")?;
        if result.request_id != request_id
            || result.document != tool.document
            || result.workspace_root != tool.workspace_root
        {
            return Err("debug discovery source changed".into());
        }
        // A project without marker files debugs from its workspace root, which `cwd` already holds.
        if let Some(root) = result.project_root {
            launch.cwd = root;
        }
        launch.phase = Phase::Adapter;
        Ok(())
    }
    pub(in crate::app) fn debug_preflight(&mut self, ready: ExecutionReady) -> Result<(), String> {
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
            // Python virtual environments depend on the invoked path, not just its target.
            // Preflight validates that target; keep the adapter's original invocation path.
            launch.executable =
                Some(std::path::absolute(&request.executable).map_err(|error| error.to_string())?);
            launch.cwd = result.cwd;
            if let Some(program) = &mut launch.program {
                if !program.is_absolute() {
                    *program = result.root.join(&*program);
                }
                if launch.source_program {
                    let target = crate::files::canonicalize(&program).map_err(|_| {
                        format!("debug program {} does not exist; build it in the terminal (Ctrl-/), then press Space d s", program.display())
                    })?;
                    if !launch
                        .roots
                        .iter()
                        .any(|r| target.starts_with(&r.canonical_path))
                    {
                        return Err("debug program is outside every workspace root".into());
                    }
                    *program = target;
                    launch.phase = Phase::Start;
                } else {
                    launch.phase = Phase::Program;
                }
            } else {
                launch.phase = Phase::Start;
            }
        }
        Ok(())
    }
    /// Owner, immutable policy/catalog and actual workspace transport capability stay
    /// borrowed through request construction, real submission and registration.
    pub(in crate::app) fn advance_debug(
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
            return Err("debug launch source is stale".into());
        }
        match launch.phase {
            Phase::Requested | Phase::Discovering(_) | Phase::Preflight { .. } => {}
            Phase::Discovery => {
                if !lane.discovery_available() {
                    self.launch = Some(launch);
                    return Ok(None);
                }
                let tool = launch.tool.as_ref().ok_or("debug tool discovery missing")?;
                let request_id = lane.discover(
                    tool.document.clone(),
                    tool.root,
                    tool.definition.project_root_markers.clone(),
                    tool.definition.project_root_strategy,
                )?;
                launch.phase = Phase::Discovering(request_id);
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
                // Read the identity FIRST: the request construction below moves fields out of the
                // intent, and the identity must describe what was actually asked for.
                let target = launch.identity();
                let breakpoints = self.startup_breakpoints();
                let reservation = self.service.reservation().map_err(|e| e.to_string())?;
                let mut paths = vec![launch.cwd.clone()];
                if let Some(program) = &launch.program {
                    paths.push(program.clone());
                }
                let mut arguments = serde_json::Map::from_iter(launch.session.arguments);
                arguments.insert("cwd".into(), serde_json::json!(launch.cwd));
                // As in VS Code, or `input()` gets end of file.
                let adapter_runs_terminal = launch.configuration.is_none()
                    && matches!(
                        launch.session.adapter.as_str(),
                        "debugpy" | "delve" | "codelldb" | "js-debug-adapter"
                    );
                // CodeLLDB asks for a terminal without being told to.
                match launch.session.adapter.as_str() {
                    "debugpy" if adapter_runs_terminal => {
                        arguments.insert("console".into(), serde_json::json!("integratedTerminal"));
                    }
                    "delve" if adapter_runs_terminal => {
                        launch
                            .adapter
                            .environment
                            .insert(crate::debugger::ADAPTER_IN_TERMINAL.into(), "1".into());
                    }
                    _ => {}
                }
                // A project's virtual environment is what its imports need, as in VS Code.
                let venv = launch.cwd.join(".venv/bin/python");
                if launch.session.adapter == "debugpy"
                    && venv.exists()
                    && !arguments.contains_key("python")
                {
                    arguments.insert("python".into(), serde_json::json!([venv]));
                }
                // A debug build has no use for VCS stamping, and it fails the build when Git can't
                // read the repository, such as one owned by another user.
                if launch.session.adapter == "delve" {
                    arguments
                        .entry("buildFlags")
                        .or_insert_with(|| serde_json::json!("-buildvcs=false"));
                    // Delve builds `__debug_bin*` in the project and only deletes it when it
                    // exits cleanly; the temporary directory keeps a stopped session's out of it.
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_nanos());
                    arguments.entry("output").or_insert_with(|| {
                        serde_json::json!(std::env::temp_dir().join(format!("dust-debug-{stamp}")))
                    });
                }
                if let Some(program) = launch.program {
                    // Go builds packages, so a file beside the active one still has to compile.
                    let program = match program.parent() {
                        Some(package)
                            if launch.session.adapter == "delve" && launch.automatic_program =>
                        {
                            package.to_owned()
                        }
                        _ => program,
                    };
                    arguments.insert("program".into(), serde_json::json!(program));
                }
                reservation
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
                            breakpoints,
                        },
                        Some(ExecutionScope {
                            roots: launch.roots,
                            paths,
                        }),
                    )
                    .map_err(|e| e.to_string())?;
                // The session now KNOWS what it is attached to: retain that identity for as long
                // as the session lives, rather than dropping it with the intent.
                self.configuration = launch.configuration;
                self.terminal_profile = launch.terminal;
                self.adapter_runs_terminal = adapter_runs_terminal;
                self.scope = launch.scope;
                self.target = Some(target);
                self.choices = None;
                self.source = None;
                return Ok(Some(launch.name));
            }
        }
        self.launch = Some(launch);
        Ok(None)
    }
    #[cfg(test)]
    pub(in crate::app) fn debug_program_pending(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase, Phase::Preflight { program: true, .. }))
    }
    #[cfg(test)]
    pub(in crate::app) fn debug_discovering(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|l| matches!(l.phase, Phase::Discovering(_)))
    }
    #[cfg(test)]
    pub(in crate::app) fn debug_preflight_paths(&self) -> Option<(&Path, &Path)> {
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
    use crate::app::debugger::launch::*;
    #[test]
    fn restart_uses_original_source_and_validates_before_stopping() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), RootIdentity::capture(root.path()).unwrap())],
        )
        .unwrap();
        let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
        let policy = DebugConfig::default();
        let terminals = crate::config::TerminalsConfig::default();
        let mut owner = Debugger::new(&policy, workspace.version());
        let (service, _adapter) = crate::debugger::test_session();
        owner.service = service;
        let path = root.path().join("original.py");
        std::fs::write(&path, "print(1)").unwrap();
        owner.target = Some(crate::app::debugger::DapTargetIdentity {
            configuration: None,
            adapter: "debugpy".into(),
            request: DebugRequestKind::Launch,
            program: Some(path.clone()),
            automatic_program: true,
            document: Some(path.clone()),
            target: None,
        });
        assert!(
            owner
                .request_restart((&policy, &terminals, 1), &workspace, &[])
                .is_err()
        );
        assert!(owner.service.is_running());
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|definition| definition.id == "debugpy")
            .unwrap();
        let tools = [ToolEntry {
            definition,
            status: ToolStatus::Installed("/bin/false".into()),
        }];
        owner
            .request_restart((&policy, &terminals, 1), &workspace, &tools)
            .unwrap();
        assert_eq!(
            owner
                .launch
                .as_ref()
                .unwrap()
                .tool
                .as_ref()
                .unwrap()
                .document,
            path
        );
        assert_eq!(owner.launch.as_ref().unwrap().cwd, root.path());
        owner.stop().unwrap();
        owner
            .request_restart((&policy, &terminals, 1), &workspace, &tools)
            .expect("restart after an explicit stop");
        assert!(owner.launch.is_some());
        owner.stop().unwrap();
        let (service, _adapter) = crate::debugger::test_session();
        owner.service = service;
        // The application's terminated-event handler stops the service directly.
        owner.service.stop().unwrap();
        owner
            .request_restart((&policy, &terminals, 1), &workspace, &tools)
            .expect("restart after normal termination");
        assert!(owner.launch.is_some());
    }

    #[test]
    fn relative_programs_use_the_source_workspace_root_and_cannot_escape_it() {
        let root = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(root.path()).unwrap();
        let root_path = identity.canonical_path.clone();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let workspace = Workspace::new(state, root.path().join("trust"), 1, false, false, 1);
        let policy = DebugConfig::default();
        let terminals = crate::config::TerminalsConfig::default();
        let mut owner = Debugger::new(&policy, workspace.version());
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|d| d.id == "codelldb")
            .unwrap();
        let tools = [ToolEntry {
            definition,
            status: ToolStatus::Installed(std::env::current_exe().unwrap()),
        }];
        let source = root_path.join("main.rs");
        owner
            .request_debug(
                (None, Some("codelldb")),
                Some("target/debug/app".into()),
                Some((&source, "rust")),
                (&policy, &terminals, 1),
                &workspace,
                &tools,
            )
            .unwrap();
        assert_eq!(
            owner.launch.as_ref().unwrap().program,
            Some(root_path.join("target/debug/app"))
        );
        assert!(
            owner
                .request_debug(
                    (None, Some("codelldb")),
                    Some("../outside".into()),
                    Some((&source, "rust")),
                    (&policy, &terminals, 1),
                    &workspace,
                    &tools,
                )
                .unwrap_err()
                .contains("outside its original root")
        );
    }

    pub(in crate::app::debugger) fn original_launch_fixture() {
        use crate::app::workspace::DiscoveryKind;
        use crate::app::workspace::WorkspacePreflightEvent;
        use std::time::Duration;
        use std::time::Instant;
        let directory = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(directory.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity.clone())],
        )
        .unwrap();
        let mut workspace =
            Workspace::new(state, directory.path().join("trust"), 2, false, false, 2);
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
        let mut owner = Debugger::new(&policy, workspace.version());
        let terminals = crate::config::TerminalsConfig::default();
        let old = owner
            .request_debug(
                (Some("owned"), None),
                None,
                None,
                (&policy, &terminals, 1),
                &workspace,
                &[],
            )
            .unwrap();
        let fresh = owner
            .request_debug(
                (Some("owned"), None),
                None,
                None,
                (&policy, &terminals, 1),
                &workspace,
                &[],
            )
            .unwrap();
        assert_ne!(old, fresh);
        let captured = owner.launch.as_ref().unwrap().terminal.as_ref().unwrap();
        assert_eq!(captured.name, "shell");
        assert_eq!(captured.cwd, directory.path());
        assert_eq!(captured.profile, terminals.profiles["shell"]);
        assert!(owner.authorize_debug(old, &workspace).is_err());
        owner.authorize_debug(fresh.clone(), &workspace).unwrap();
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
        let mut foreign = Debugger::new(&policy, workspace.version());
        assert!(foreign.authorize_debug(fresh, &workspace).is_err());
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
                (Some("owned"), None),
                None,
                None,
                (&policy, &terminals, 1),
                &workspace,
                &[],
            )
            .unwrap();
        owner.authorize_debug(intent, &workspace).unwrap();
        assert!(
            owner
                .advance_debug(
                    workspace.execution_lane(DiscoveryKind::Debugger),
                    &policy,
                    2,
                    &[]
                )
                .is_err(),
            "configuration revision substitution accepted original launch"
        );
        assert!(owner.launch.is_none());
        owner.begin_shutdown();
        assert!(
            owner
                .request_debug(
                    (Some("owned"), None),
                    None,
                    None,
                    (&policy, &terminals, 2),
                    &workspace,
                    &[]
                )
                .is_err()
        );
    }

    #[test]
    fn attach_preserves_the_target_process_identity_it_was_given() {
        use crate::app::workspace::DiscoveryKind;
        use crate::app::workspace::WorkspacePreflightEvent;
        use std::time::Duration;
        use std::time::Instant;
        let directory = tempfile::tempdir().unwrap();
        let identity = RootIdentity::capture(directory.path()).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity.clone())],
        )
        .unwrap();
        let mut workspace =
            Workspace::new(state, directory.path().join("trust"), 2, false, false, 2);
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
        // An ATTACH names the target it was given, and carries NO program: the editor did not
        // start that process.
        policy.sessions.insert(
            "attached".into(),
            DebugSessionConfig {
                adapter: "owned".into(),
                request: DebugRequestKind::Attach,
                cwd: None,
                program: None,
                run_in_terminal_profile: None,
                arguments: BTreeMap::from([("processId".into(), serde_json::json!(4242))]),
                path_maps: BTreeMap::new(),
            },
        );
        let mut owner = Debugger::new(&policy, workspace.version());
        let terminals = crate::config::TerminalsConfig::default();
        let intent = owner
            .request_debug(
                (Some("attached"), None),
                None,
                None,
                (&policy, &terminals, 1),
                &workspace,
                &[],
            )
            .unwrap();
        owner.authorize_debug(intent, &workspace).unwrap();
        owner
            .advance_debug(
                workspace.execution_lane(DiscoveryKind::Debugger),
                &policy,
                1,
                &[],
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = workspace.preflight_events();
            if !events.is_empty() {
                for event in events {
                    match event {
                        WorkspacePreflightEvent::Ready(ready) => {
                            owner.debug_preflight(ready).unwrap();
                        }
                        _ => panic!("the attach preflight failed"),
                    }
                }
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        // The request GOES OUT here, and the target's identity must survive it.
        owner
            .advance_debug(
                workspace.execution_lane(DiscoveryKind::Debugger),
                &policy,
                1,
                &[],
            )
            .unwrap();
        let target = owner
            .target()
            .expect("the target identity is RETAINED once the request goes out");
        assert_eq!(
            target.request,
            DebugRequestKind::Attach,
            "the session knows it ATTACHED rather than launched"
        );
        assert_eq!(
            target.target.as_deref(),
            Some("processId 4242"),
            "the target named is the one the configuration was GIVEN, not one the editor invented"
        );
        assert_eq!(
            target.program, None,
            "an attached target is NOT presented as a program the editor launched"
        );
    }

    #[test]
    fn configured_targets_and_automatic_delve_restarts_reach_the_adapter() {
        use crate::app::workspace::DiscoveryKind;
        use crate::app::workspace::WorkspacePreflightEvent;
        use std::time::Duration;
        use std::time::Instant;
        for (adapter_id, directory, automatic) in [
            ("delve", true, false),
            ("delve", false, false),
            ("my-debugpy", false, false),
            ("delve", false, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let root_path = crate::files::canonicalize(root.path()).unwrap();
            let target = root_path.join(if automatic { "main.go" } else { "target" });
            if directory {
                std::fs::create_dir(&target).unwrap();
            } else {
                std::fs::write(&target, "executable target").unwrap();
            }
            let script = root.path().join("adapter.py");
            std::fs::write(&script, crate::debugger::TEST_ADAPTER).unwrap();
            let identity = RootIdentity::capture(root.path()).unwrap();
            let state = crate::workspace::WorkspaceState::new(
                crate::workspace::WorkspaceId(1),
                [("root".into(), identity.clone())],
            )
            .unwrap();
            let mut workspace =
                Workspace::new(state, root.path().join("trust"), 2, false, false, 2);
            workspace
                .fixture_identity(RootId(1), identity.clone())
                .unwrap();
            workspace.fixture_trust_identity(identity);
            workspace.reconcile_trust();
            let python = crate::debugger::test_python();
            #[cfg(unix)]
            let python = {
                let invoked = root_path.join("venv/bin/python");
                std::fs::create_dir_all(invoked.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(python, &invoked).unwrap();
                invoked
            };
            let mut policy = DebugConfig::default();
            policy.adapters.insert(
                adapter_id.into(),
                DebugAdapterConfig {
                    program: python.clone(),
                    arguments: vec!["-u".into(), script.display().to_string()],
                    environment: BTreeMap::new(),
                },
            );
            policy.sessions.insert(
                "configured".into(),
                DebugSessionConfig {
                    adapter: adapter_id.into(),
                    request: DebugRequestKind::Launch,
                    cwd: None,
                    program: Some(target.clone()),
                    run_in_terminal_profile: None,
                    arguments: BTreeMap::new(),
                    path_maps: BTreeMap::new(),
                },
            );
            let mut definition = crate::tools::bundled_tool_catalog()
                .into_iter()
                .find(|d| d.id == "delve")
                .unwrap();
            definition.launch_args = vec!["-u".into(), script.display().to_string()];
            let tools = [ToolEntry {
                definition,
                status: ToolStatus::Installed(python.clone()),
            }];
            let mut owner = Debugger::new(&policy, workspace.version());
            let terminals = crate::config::TerminalsConfig::default();
            let intent = owner
                .request_debug(
                    (
                        if automatic { None } else { Some("configured") },
                        Some(adapter_id),
                    ),
                    None,
                    Some((&target, "go")),
                    (&policy, &terminals, 1),
                    &workspace,
                    &tools,
                )
                .unwrap();
            for run in 0..if automatic { 2 } else { 1 } {
                let intent = if run == 0 {
                    intent.clone()
                } else {
                    owner
                        .request_restart((&policy, &terminals, 1), &workspace, &tools)
                        .unwrap()
                };
                owner.authorize_debug(intent, &workspace).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(result) = workspace.project_result(DiscoveryKind::Debugger) {
                        owner.debug_root_result(result).unwrap();
                    }
                    for event in workspace.preflight_events() {
                        match event {
                            WorkspacePreflightEvent::Ready(ready) => {
                                owner.debug_preflight(ready).unwrap();
                                assert_eq!(
                                    owner.launch.as_ref().unwrap().executable.as_ref(),
                                    Some(&python),
                                    "adapter invocation must preserve a virtual environment path"
                                );
                            }
                            _ => panic!("preflight failed"),
                        }
                    }
                    owner
                        .advance_debug(
                            workspace.execution_lane(DiscoveryKind::Debugger),
                            &policy,
                            1,
                            &tools,
                        )
                        .unwrap();
                    match owner.try_event() {
                        Some(crate::debugger::DapEvent::Stopped { .. }) => break,
                        Some(crate::debugger::DapEvent::Failed { message, .. }) => {
                            panic!("{message}")
                        }
                        _ => {}
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(
                    std::fs::read_to_string(root_path.join("launch-program")).unwrap(),
                    if automatic { &root_path } else { &target }
                        .display()
                        .to_string()
                );
                owner.stop().unwrap();
            }
        }
    }
}
