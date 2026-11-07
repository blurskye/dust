//! Root discovery and execution metadata belong to workspace, not protocol adapters.
use crate::app::workspace::Workspace;
use crate::app::workspace::WorkspaceVersion;
use crate::files::ProjectRootResolution;
use crate::files::ProjectRootStrategy;
use crate::files::ProjectRootWorker;
use crate::files::RootIdentity;
use crate::process::ExecutionPreflightEvent;
use crate::process::ExecutionPreflightRequest;
use crate::process::ExecutionPreflightWorker;
use crate::process::PreflightError;
use crate::process::PreflightId;
use crate::workspace::RootId;
use std::collections::HashMap;
use std::path::PathBuf;

/// Actual workspace-worker result. Raw compatibility results cannot authorize task spawn.
pub(in crate::app) struct ExecutionReady {
    result: crate::process::ExecutionPreflightResult,
    request: ExecutionPreflightRequest,
    source: WorkspaceVersion,
}
pub(in crate::app) enum WorkspacePreflightEvent {
    Ready(ExecutionReady),
    Failed {
        id: PreflightId,
        root_revision: u64,
        message: String,
    },
}
impl ExecutionReady {
    pub(in crate::app) fn result(&self) -> &crate::process::ExecutionPreflightResult {
        &self.result
    }
    pub(in crate::app) fn into_execution_result(
        self,
        request: &ExecutionPreflightRequest,
        workspace: &Workspace,
    ) -> Result<crate::process::ExecutionPreflightResult, String> {
        self.into_scoped_result(request, &workspace.version())
    }
    pub(in crate::app) fn into_scoped_result(
        self,
        request: &ExecutionPreflightRequest,
        source: &WorkspaceVersion,
    ) -> Result<crate::process::ExecutionPreflightResult, String> {
        if &self.source != source
            || self.request.executable != request.executable
            || self.request.cwd != request.cwd
            || self.request.roots != request.roots
            || self.request.root_revision != request.root_revision
        {
            return Err("execution preflight does not match its original workspace request".into());
        }
        Ok(self.result)
    }
    #[cfg(test)]
    pub(in crate::app) fn fixture(
        workspace: &Workspace,
        result: crate::process::ExecutionPreflightResult,
    ) -> Self {
        Self {
            request: ExecutionPreflightRequest {
                executable: result.executable.clone(),
                cwd: result.cwd.clone(),
                roots: vec![result.root.clone()],
                root_revision: result.root_revision,
            },
            result,
            source: workspace.version(),
        }
    }
}
#[derive(Clone, Copy)]
pub(in crate::app) enum DiscoveryKind {
    Language,
    Debugger,
}
#[derive(Default)]
pub(super) struct Discovery {
    worker: ProjectRootWorker,
    pending: Option<DiscoveryIntent>,
}
struct DiscoveryIntent {
    request_id: u64,
    source: WorkspaceVersion,
    root: RootId,
    identity: RootIdentity,
    document: PathBuf,
}
pub(super) struct Preflights {
    worker: ExecutionPreflightWorker,
    pending: HashMap<PreflightId, (WorkspaceVersion, ExecutionPreflightRequest)>,
    capacity: usize,
}
impl Preflights {
    pub(in crate::app) fn new(capacity: usize) -> Self {
        Self {
            worker: ExecutionPreflightWorker::start(capacity),
            pending: HashMap::new(),
            capacity: capacity.clamp(1, 256),
        }
    }
}
impl Workspace {
    fn discovery(&mut self, kind: DiscoveryKind) -> &mut Discovery {
        match kind {
            DiscoveryKind::Language => &mut self.language_roots,
            DiscoveryKind::Debugger => &mut self.debugger_roots,
        }
    }
    #[cfg(test)]
    pub(in crate::app) fn discover_project(
        &mut self,
        kind: DiscoveryKind,
        document: PathBuf,
        root: RootId,
        markers: Vec<String>,
        strategy: ProjectRootStrategy,
    ) -> Result<u64, String> {
        self.execution_lane(kind)
            .discover(document, root, markers, strategy)
    }

    pub(in crate::app) fn project_result(
        &mut self,
        kind: DiscoveryKind,
    ) -> Option<Result<ProjectRootResolution, String>> {
        let discovery = self.discovery(kind);
        let result = discovery.worker.poll_result()?;
        let intent = discovery.pending.take()?;
        Some(result.map_err(|e| e.to_string()).and_then(|result| {
            if intent.source != self.version
                || result.request_id != intent.request_id
                || result.document != intent.document
                || result.workspace_root != intent.identity.canonical_path
                || !self
                    .state
                    .roots()
                    .iter()
                    .any(|r| r.id == intent.root && r.identity == intent.identity)
            {
                return Err("project-root discovery source changed".into());
            }
            Ok(result)
        }))
    }
    #[cfg(test)]
    pub(in crate::app) fn request_preflight(
        &mut self,
        request: ExecutionPreflightRequest,
    ) -> Result<PreflightId, PreflightError> {
        self.execution_lane(DiscoveryKind::Language)
            .preflight(request)
    }

    #[cfg(test)]
    pub(in crate::app) fn preflight_pending(&self) -> usize {
        self.preflights.pending.len()
    }
    pub(in crate::app) fn preflight_events(&mut self) -> Vec<WorkspacePreflightEvent> {
        let events = match self.preflights.worker.poll() {
            Ok(events) => events,
            Err(error) => {
                return self
                    .preflights
                    .pending
                    .drain()
                    .map(|(id, _)| WorkspacePreflightEvent::Failed {
                        id,
                        root_revision: self.state.root_revision(),
                        message: error.to_string(),
                    })
                    .collect();
            }
        };
        let mut accepted = Vec::with_capacity(events.len());
        for event in events {
            let id = match &event {
                ExecutionPreflightEvent::Ready(r) => r.id,
                ExecutionPreflightEvent::Failed { id, .. } => *id,
            };
            let Some((source, request)) = self.preflights.pending.remove(&id) else {
                continue;
            };
            if source != self.version
                || self.safe_mode
                || self.state.trust() != crate::workspace::WorkspaceTrust::Trusted
            {
                accepted.push(WorkspacePreflightEvent::Failed {
                    id,
                    root_revision: self.root_revision(),
                    message: "execution preflight workspace source changed".into(),
                });
            } else {
                accepted.push(match event {
                    ExecutionPreflightEvent::Ready(result) => {
                        WorkspacePreflightEvent::Ready(ExecutionReady {
                            result,
                            request,
                            source,
                        })
                    }
                    ExecutionPreflightEvent::Failed {
                        id,
                        root_revision,
                        message,
                    } => WorkspacePreflightEvent::Failed {
                        id,
                        root_revision,
                        message,
                    },
                });
            }
        }
        accepted
    }
}

/// Borrowed transport capability: no workspace mutation or trust-granting API.
pub(in crate::app) struct ExecutionLane<'a> {
    discovery: &'a mut Discovery,
    preflights: &'a mut Preflights,
    roots: &'a [crate::workspace::WorkspaceRoot],
    source: WorkspaceVersion,
    revision: u64,
    allowed: bool,
}
impl Workspace {
    pub(in crate::app) fn execution_lane(&mut self, kind: DiscoveryKind) -> ExecutionLane<'_> {
        let discovery = match kind {
            DiscoveryKind::Language => &mut self.language_roots,
            DiscoveryKind::Debugger => &mut self.debugger_roots,
        };
        ExecutionLane {
            discovery,
            preflights: &mut self.preflights,
            roots: self.state.roots(),
            source: self.version.clone(),
            revision: self.state.root_revision(),
            allowed: !self.safe_mode
                && self.state.trust() == crate::workspace::WorkspaceTrust::Trusted,
        }
    }
}
impl ExecutionLane<'_> {
    pub(in crate::app) fn request(
        &self,
        executable: PathBuf,
        cwd: PathBuf,
    ) -> ExecutionPreflightRequest {
        ExecutionPreflightRequest {
            executable,
            cwd,
            roots: self
                .roots
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            root_revision: self.revision,
        }
    }
    pub(in crate::app) fn discovery_available(&self) -> bool {
        self.discovery.pending.is_none()
    }
    pub(in crate::app) fn root_revision(&self) -> u64 {
        self.revision
    }
    pub(in crate::app) fn current(&self, source: &WorkspaceVersion) -> bool {
        self.allowed && &self.source == source
    }
    pub(in crate::app) fn preflight_available(&self) -> Result<bool, String> {
        if !self.preflights.worker.identity_available() {
            return Err("execution preflight identity exhausted".into());
        }
        Ok(self.allowed && self.preflights.pending.len() < self.preflights.capacity)
    }
    pub(in crate::app) fn discover(
        &mut self,
        document: PathBuf,
        root: RootId,
        markers: Vec<String>,
        strategy: ProjectRootStrategy,
    ) -> Result<u64, String> {
        let identity = self
            .roots
            .iter()
            .find(|r| r.id == root)
            .ok_or("discovery root is closed")?
            .identity
            .clone();
        if !document.starts_with(&identity.canonical_path) {
            return Err("discovery document is outside its root".into());
        }
        let source = self.source.clone();
        let revision = self.revision;
        let discovery = &mut *self.discovery;
        // A detached old probe is not a cancelled filesystem call. Keep one
        // accepted probe per protocol lane until it produces a terminal result.
        if discovery.pending.is_some() {
            return Err("project-root discovery is already pending".into());
        }
        let request_id = discovery
            .worker
            .resolve(
                document.clone(),
                identity.canonical_path.clone(),
                revision,
                markers,
                strategy,
            )
            .map_err(|e| e.to_string())?;
        discovery.pending = Some(DiscoveryIntent {
            request_id,
            source,
            root,
            identity,
            document,
        });
        Ok(request_id)
    }
    pub(in crate::app) fn preflight(
        &mut self,
        mut request: ExecutionPreflightRequest,
    ) -> Result<PreflightId, PreflightError> {
        if request.root_revision != self.revision || !self.allowed {
            return Err(PreflightError::Invalid);
        }
        if self.preflights.pending.len() >= self.preflights.capacity {
            return Err(PreflightError::QueueFull);
        }
        let identities = self
            .roots
            .iter()
            .map(|r| r.identity.clone())
            .collect::<Vec<_>>();
        if identities.iter().any(|r| r.filesystem_identity.is_none()) {
            return Err(PreflightError::Invalid);
        }
        request.roots = identities
            .iter()
            .map(|r| r.canonical_path.clone())
            .collect();
        let original = request.clone();
        let id = self.preflights.worker.request_scoped(request, identities)?;
        self.preflights
            .pending
            .insert(id, (self.source.clone(), original));
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use crate::app::workspace::discovery::*;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;
    pub(in crate::app) fn owner(root: &std::path::Path) -> Workspace {
        let identity = RootIdentity::capture(root).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity.clone())],
        )
        .unwrap();
        let mut owner = Workspace::new(state, root.join("trust.toml"), 2, false, false, 2);
        owner.fixture_identity(RootId(1), identity.clone()).unwrap();
        owner.fixture_trust_identity(identity);
        owner.reconcile_trust();
        owner
    }
    #[test]
    fn discovery_is_bounded_exact_once_and_rejects_stale_workspace_versions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='fixture'\n",
        )
        .unwrap();
        let file = directory.path().join("lib.rs");
        std::fs::write(&file, "fn main(){}\n").unwrap();
        let mut owner = owner(directory.path());
        let id = owner
            .discover_project(
                DiscoveryKind::Language,
                file.clone(),
                RootId(1),
                vec!["Cargo.toml".into()],
                ProjectRootStrategy::Cargo,
            )
            .unwrap();
        assert!(
            owner
                .discover_project(
                    DiscoveryKind::Language,
                    file,
                    RootId(1),
                    vec![],
                    ProjectRootStrategy::Marker
                )
                .is_err()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(result) = owner.project_result(DiscoveryKind::Language) {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(result.request_id, id);
        assert_eq!(result.project_root.as_deref(), Some(directory.path()));
        assert!(owner.project_result(DiscoveryKind::Language).is_none());
        owner
            .discover_project(
                DiscoveryKind::Debugger,
                result.document,
                RootId(1),
                vec!["Cargo.toml".into()],
                ProjectRootStrategy::Cargo,
            )
            .unwrap();
        owner.fixture_safe_mode(true);
        loop {
            if let Some(result) = owner.project_result(DiscoveryKind::Debugger) {
                assert!(result.is_err());
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn preflight_rejects_same_path_replacement_even_without_an_observed_root_revision_change() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let mut owner = owner(&root);
        std::fs::rename(&root, directory.path().join("old-root")).unwrap();
        std::fs::create_dir(&root).unwrap();
        let id = owner
            .request_preflight(ExecutionPreflightRequest {
                executable: std::env::current_exe().unwrap(),
                cwd: root.clone(),
                roots: vec![root],
                root_revision: owner.root_revision(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = owner.preflight_events();
            if !events.is_empty() {
                assert!(
                    matches!(&events[..],[WorkspacePreflightEvent::Failed{id:actual,message,..}] if *actual==id && message.contains("filesystem identity changed"))
                );
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(owner.preflight_events().is_empty());
        assert!(owner.preflights.pending.is_empty());
    }
}
