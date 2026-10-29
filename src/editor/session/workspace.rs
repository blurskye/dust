//! Workspace roots, exact trust identities and asynchronous identity validation.
//! Feature execution payloads remain with their callers, not in this owner.
mod discovery;
use crate::workspace::{
    ExecutionSource, RootId, RootIdentity, TrustEvent, TrustStore, WorkspaceError, WorkspaceRoot,
    WorkspaceState, WorkspaceTrust, WorkspaceTrustService,
};
pub(in crate::editor::session) use discovery::{
    DiscoveryKind, ExecutionLane, ExecutionReady, WorkspacePreflightEvent,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct WorkspaceVersion(Arc<()>);
impl PartialEq for WorkspaceVersion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for WorkspaceVersion {}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct ValidationId(u64);
pub(in crate::editor::session) enum WorkspaceFailure {
    Message(&'static str),
    Diagnostic(&'static str, String),
    Plain(String),
}
impl From<WorkspaceError> for WorkspaceFailure {
    fn from(e: WorkspaceError) -> Self {
        Self::Plain(e.to_string())
    }
}
pub(in crate::editor::session) enum WorkspaceUpdate {
    Message(&'static str),
    Failure(WorkspaceFailure),
    RootAdded(PathBuf),
    /// The editor moved from the `old` root to the new `path`.
    RootSwitched { path: PathBuf, old: PathBuf },
    ScopeInvalidated,
    Validated {
        id: ValidationId,
        permit: Option<ExecutionPermit>,
    },
}
pub(in crate::editor::session) struct ExecutionPermit {
    id: ValidationId,
    source: WorkspaceVersion,
}
pub(in crate::editor::session) struct DebugPermit {
    permit: ExecutionPermit,
    id: ValidationId,
    nonce: Arc<()>,
}
pub(in crate::editor::session) struct TaskPermit {
    permit: ExecutionPermit,
    id: ValidationId,
    name: String,
    kind: crate::config::TaskKind,
}
pub(in crate::editor::session) struct TerminalPermit {
    nonce: Arc<()>,
    permit: ExecutionPermit,
    id: ValidationId,
    profile: String,
}
struct RootOperation {
    generation: u64,
    source: WorkspaceVersion,
    requested_path: PathBuf,
    /// The root this one replaces when switching worktrees.
    replaces: Option<RootId>,
}
struct Validation {
    id: ValidationId,
    source: WorkspaceVersion,
    failed: bool,
    generations: HashMap<u64, RootIdentity>,
}
struct Confirmation {
    source: WorkspaceVersion,
    identities: Vec<RootIdentity>,
}
struct Persist {
    generation: u64,
    source: WorkspaceVersion,
    target: Option<WorkspaceTrust>,
}
pub(super) struct WorkspaceSession {
    state: WorkspaceState,
    version: WorkspaceVersion,
    service: WorkspaceTrustService,
    store: TrustStore,
    state_path: PathBuf,
    load_generation: Option<u64>,
    identity_generation: Option<(u64, RootId, WorkspaceVersion)>,
    persist: Option<Persist>,
    database_ready: bool,
    identity_ready: bool,
    startup_prompt_checked: bool,
    safe_mode: bool,
    root_operation: Option<RootOperation>,
    validation: Option<Validation>,
    next_validation: Option<u64>,
    confirmation: Option<Confirmation>,
    language_roots: discovery::Discovery,
    debugger_roots: discovery::Discovery,
    preflights: discovery::Preflights,
    /// The tutor's disposable fixture, if an exercise surface is open. It is a sandbox, not a
    /// workspace root: it is never entered into `Roots`, never trusted and never persisted.
    tutor_service: crate::workspace::TutorService,
    tutor_fixture: Option<crate::workspace::TutorFixture>,
    tutor_generation: Option<crate::workspace::TutorGeneration>,
    /// What the user has actually DONE in the sandbox, observed from real outcomes. Held here
    /// rather than in InteractiveApplication, whose field count is asserted.
    tutor_progress: crate::workspace::TutorProgress,
}
impl WorkspaceSession {
    pub(in crate::editor::session) fn edit_scope(&self) -> &WorkspaceState {
        &self.state
    }
    pub(in crate::editor::session) fn new(
        state: WorkspaceState,
        state_path: PathBuf,
        capacity: usize,
        initialize: bool,
        safe_mode: bool,
        preflight_capacity: usize,
    ) -> Self {
        let mut service = WorkspaceTrustService::start(capacity);
        let version = WorkspaceVersion(Arc::new(()));
        let load_generation = initialize
            .then(|| service.load(state_path.clone()).ok())
            .flatten();
        let identity_generation = if initialize {
            state
                .active_root()
                .and_then(|id| state.roots().iter().find(|r| r.id == id))
                .and_then(|r| {
                    service
                        .inspect_root(r.identity.canonical_path.clone())
                        .ok()
                        .map(|g| (g, r.id, version.clone()))
                })
        } else {
            None
        };
        Self {
            state,
            version,
            service,
            store: TrustStore::default(),
            state_path,
            load_generation,
            identity_generation,
            persist: None,
            database_ready: false,
            identity_ready: false,
            startup_prompt_checked: false,
            safe_mode,
            root_operation: None,
            validation: None,
            next_validation: Some(0),
            confirmation: None,
            language_roots: discovery::Discovery::default(),
            debugger_roots: discovery::Discovery::default(),
            preflights: discovery::Preflights::new(preflight_capacity),
            tutor_service: crate::workspace::TutorService::start(capacity),
            tutor_fixture: None,
            tutor_generation: None,
            tutor_progress: crate::workspace::TutorProgress::default(),
        }
    }
    pub(in crate::editor::session) fn debug_permit(
        &self,
        permit: ExecutionPermit,
        id: ValidationId,
        nonce: Arc<()>,
    ) -> Option<DebugPermit> {
        self.permit_current(&permit, id)
            .then_some(DebugPermit { permit, id, nonce })
    }
    pub(in crate::editor::session) fn consume_debug_permit(
        &self,
        permit: DebugPermit,
        nonce: &Arc<()>,
    ) -> bool {
        Arc::ptr_eq(&permit.nonce, nonce) && self.consume_permit(permit.permit, permit.id)
    }
    pub(in crate::editor::session) fn terminal_permit(
        &self,
        permit: ExecutionPermit,
        id: ValidationId,
        profile: String,
        nonce: Arc<()>,
    ) -> Option<TerminalPermit> {
        self.permit_current(&permit, id).then_some(TerminalPermit {
            nonce,
            permit,
            id,
            profile,
        })
    }
    pub(in crate::editor::session) fn consume_terminal_permit(
        &self,
        permit: TerminalPermit,
        profile: &str,
        nonce: &Arc<()>,
    ) -> bool {
        Arc::ptr_eq(&permit.nonce, nonce)
            && permit.profile == profile
            && self.consume_permit(permit.permit, permit.id)
    }
    pub(in crate::editor::session) fn task_permit(
        &self,
        permit: ExecutionPermit,
        id: ValidationId,
        name: String,
        kind: crate::config::TaskKind,
    ) -> Option<TaskPermit> {
        self.permit_current(&permit, id).then_some(TaskPermit {
            permit,
            id,
            name,
            kind,
        })
    }
    pub(in crate::editor::session) fn consume_task_permit(
        &self,
        permit: TaskPermit,
        name: &str,
        kind: crate::config::TaskKind,
    ) -> bool {
        permit.name == name && permit.kind == kind && self.consume_permit(permit.permit, permit.id)
    }
    pub(in crate::editor::session) fn consume_permit(
        &self,
        permit: ExecutionPermit,
        id: ValidationId,
    ) -> bool {
        self.permit_current(&permit, id)
    }
    pub(in crate::editor::session) fn permit_current(
        &self,
        permit: &ExecutionPermit,
        id: ValidationId,
    ) -> bool {
        permit.id == id
            && permit.source == self.version
            && !self.safe_mode
            && self.state.trust() == WorkspaceTrust::Trusted
    }
    pub(in crate::editor::session) fn version(&self) -> WorkspaceVersion {
        self.version.clone()
    }
    pub(in crate::editor::session) fn roots(&self) -> &[WorkspaceRoot] {
        self.state.roots()
    }
    pub(in crate::editor::session) fn root_revision(&self) -> u64 {
        self.state.root_revision()
    }
    /// Every workspace root this machine has opened, for the project list.
    pub(in crate::editor::session) fn seen_roots(&self) -> Vec<PathBuf> {
        self.store
            .seen_identities()
            .into_iter()
            .map(|identity| identity.canonical_path)
            .collect()
    }
    pub(in crate::editor::session) fn active_root(&self) -> Option<RootId> {
        self.state.active_root()
    }
    pub(in crate::editor::session) fn trust(&self) -> WorkspaceTrust {
        self.state.trust()
    }
    pub(in crate::editor::session) fn safe_mode(&self) -> bool {
        self.safe_mode
    }
    pub(in crate::editor::session) fn root_operation_pending(&self) -> bool {
        self.root_operation.is_some()
    }
    pub(in crate::editor::session) fn persist_pending(&self) -> bool {
        self.persist.is_some()
    }
    pub(in crate::editor::session) fn require_path(
        &self,
        path: &Path,
    ) -> Result<RootId, WorkspaceError> {
        self.state.require_path(path)
    }
    pub(in crate::editor::session) fn root_for_canonical_path(
        &self,
        path: &Path,
    ) -> Option<&WorkspaceRoot> {
        self.state.root_for_canonical_path(path)
    }
    pub(in crate::editor::session) fn authorize_execution(
        &self,
        source: ExecutionSource,
    ) -> Result<(), WorkspaceError> {
        self.state.authorize_execution(source)
    }
    fn changed(&mut self) {
        self.version = WorkspaceVersion(Arc::new(()));
        self.confirmation = None;
        self.state.set_trust(WorkspaceTrust::Untrusted);
    }
    /// Ask for a disposable fixture outside every real root. The roots are passed in as they
    /// are now, so the refusal is decided against the workspace the user actually has.
    pub(in crate::editor::session) fn start_tutor_fixture(
        &mut self,
        deadline: std::time::Duration,
    ) -> Result<(), crate::workspace::TutorError> {
        let roots = self
            .state
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        let generation = self.tutor_service.create(roots, deadline)?;
        self.tutor_generation = Some(generation);
        Ok(())
    }
    /// One poll per call, exactly once: a second poll can consume and discard the event.
    pub(in crate::editor::session) fn poll_tutor(&mut self) -> Option<crate::workspace::TutorEvent> {
        let event = self.tutor_service.poll()?;
        match &event {
            crate::workspace::TutorEvent::Created { generation, result } => {
                if self.tutor_generation == Some(*generation) {
                    self.tutor_generation = None;
                    if let Ok(path) = result {
                        self.tutor_fixture = Some(crate::workspace::TutorFixture {
                            generation: *generation,
                            path: path.clone(),
                        });
                    }
                }
            }
            crate::workspace::TutorEvent::Disposed { result, .. } => {
                // The handle is dropped only when the worker reports the directory is GONE. A
                // failed disposal keeps the handle, so the surface never claims a removal that
                // did not happen and the user can try again.
                if result.is_ok() {
                    self.tutor_fixture = None;
                }
            }
        }
        Some(event)
    }
    pub(in crate::editor::session) fn tutor_fixture(&self) -> Option<&crate::workspace::TutorFixture> {
        self.tutor_fixture.as_ref()
    }
    /// Record a real action. Progress only ever comes from here, and here is only called from
    /// the outcome of an action that genuinely happened.
    pub(in crate::editor::session) fn observe_tutor_action(
        &mut self,
        action: crate::workspace::TutorAction,
    ) -> bool {
        self.tutor_progress.observe(action)
    }
    pub(in crate::editor::session) fn tutor_progress(&self) -> &crate::workspace::TutorProgress {
        &self.tutor_progress
    }
    /// The lessons this revision can actually teach. A gated lesson is returned with its missing
    /// feature so the surface can say what is not available yet rather than hiding it.
    pub(in crate::editor::session) fn tutor_lessons(
        &self,
    ) -> &'static [crate::workspace::TutorLesson] {
        crate::workspace::LESSONS
    }
    pub(in crate::editor::session) fn tutor_pending(&self) -> bool {
        self.tutor_generation.is_some()
    }
    /// Ask for the open fixture to be removed. The session drops its handle only when the
    /// worker reports the directory is gone, so a caller never believes in a disposal that did
    /// not happen.
    pub(in crate::editor::session) fn dispose_tutor_fixture(
        &mut self,
        deadline: std::time::Duration,
    ) -> Result<(), crate::workspace::TutorError> {
        let Some(fixture) = self.tutor_fixture.as_ref() else {
            return Ok(());
        };
        self.tutor_service.dispose(fixture.path.clone(), deadline)?;
        Ok(())
    }
    /// Add `path` as a root, or with `replaces`, move the editor from that root to `path`.
    pub(in crate::editor::session) fn add_root_request(
        &mut self,
        path: PathBuf,
        replaces: Option<RootId>,
    ) -> Result<(), WorkspaceFailure> {
        if self.root_operation.is_some() {
            return Err(WorkspaceFailure::Message("workspace_root_operation_busy"));
        }
        let generation = self
            .service
            .inspect_root(path.clone())
            .map_err(|e| WorkspaceFailure::Plain(e.to_string()))?;
        self.root_operation = Some(RootOperation {
            generation,
            source: self.version(),
            requested_path: path,
            replaces,
        });
        Ok(())
    }
    /// Make `new` the active root and drop `old`; returns the old root's path.
    fn switch_root(&mut self, old: RootId, new: RootId) -> Result<PathBuf, WorkspaceError> {
        self.state.set_active_root(new)?;
        let old = self.state.remove_root(old)?;
        self.changed();
        self.reconcile_trust();
        Ok(old.identity.canonical_path)
    }
    pub(in crate::editor::session) fn remove_root(
        &mut self,
        id: RootId,
    ) -> Result<WorkspaceRoot, WorkspaceError> {
        let root = self.state.remove_root(id)?;
        self.changed();
        Ok(root)
    }
    pub(in crate::editor::session) fn set_active_root(
        &mut self,
        id: RootId,
    ) -> Result<(), WorkspaceError> {
        let changed = self.state.active_root() != Some(id);
        self.state.set_active_root(id)?;
        if changed {
            self.changed();
            self.reconcile_trust();
        }
        Ok(())
    }
    pub(in crate::editor::session) fn reconcile_trust(&mut self) {
        let trusted = self.database_ready
            && self.identity_ready
            && self.state.roots().iter().all(|r| {
                r.identity.filesystem_identity.is_some() && self.store.is_trusted(&r.identity)
            });
        self.state.set_trust(if trusted {
            WorkspaceTrust::Trusted
        } else {
            WorkspaceTrust::Untrusted
        });
    }
    pub(in crate::editor::session) fn should_prompt(&mut self, input_busy: bool) -> bool {
        if self.startup_prompt_checked || !self.database_ready || !self.identity_ready || input_busy
        {
            return false;
        }
        self.startup_prompt_checked = true;
        !self.safe_mode
            && self.state.trust() != WorkspaceTrust::Trusted
            && self
                .state
                .active_root()
                .and_then(|id| self.state.roots().iter().find(|r| r.id == id))
                .is_some_and(|r| !self.store.is_seen(&r.identity))
    }
    pub(in crate::editor::session) fn cancel_confirmation(&mut self) {
        self.confirmation = None;
    }
    pub(in crate::editor::session) fn request_confirmation(
        &mut self,
    ) -> Result<(), WorkspaceFailure> {
        if !self.database_ready || !self.identity_ready {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        let identities = self
            .state
            .roots()
            .iter()
            .map(|r| r.identity.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for identity in &identities {
            changed |= self.store.mark_seen(identity.clone());
        }
        self.confirmation = Some(Confirmation {
            source: self.version(),
            identities,
        });
        if changed {
            if let Err(error) = self.persist_state(None) {
                self.confirmation = None;
                return Err(error);
            }
        }
        Ok(())
    }
    pub(in crate::editor::session) fn confirm(&mut self) -> Result<(), WorkspaceFailure> {
        if self.safe_mode {
            return Err(WorkspaceFailure::Message("safe_mode_blocked"));
        }
        let confirmation = self
            .confirmation
            .take()
            .ok_or(WorkspaceFailure::Message("workspace_trust_not_ready"))?;
        if !self.database_ready
            || !self.identity_ready
            || confirmation.source != self.version
            || confirmation.identities
                != self
                    .state
                    .roots()
                    .iter()
                    .map(|r| r.identity.clone())
                    .collect::<Vec<_>>()
        {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        if confirmation
            .identities
            .iter()
            .any(|id| id.filesystem_identity.is_none())
        {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        for identity in confirmation.identities {
            self.store.trust(identity);
        }
        self.persist_state(Some(WorkspaceTrust::Trusted))
    }
    pub(in crate::editor::session) fn revoke(&mut self) -> Result<(), WorkspaceFailure> {
        self.changed();
        for root in self.state.roots() {
            self.store.revoke(&root.identity);
        }
        if !self.database_ready {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        self.persist_state(Some(WorkspaceTrust::Untrusted))
    }
    fn persist_state(&mut self, target: Option<WorkspaceTrust>) -> Result<(), WorkspaceFailure> {
        match self.service.persist(
            self.state_path.clone(),
            self.store.identities(),
            self.store.seen_identities(),
        ) {
            Ok(generation) => {
                self.persist = Some(Persist {
                    generation,
                    source: self.version(),
                    target,
                });
                Ok(())
            }
            Err(e) => {
                self.state.set_trust(WorkspaceTrust::Untrusted);
                Err(WorkspaceFailure::Diagnostic(
                    "workspace_trust_state_failed",
                    e.to_string(),
                ))
            }
        }
    }
    pub(in crate::editor::session) fn validation_pending(&self) -> bool {
        self.validation.is_some()
    }
    pub(in crate::editor::session) fn begin_validation(
        &mut self,
    ) -> Result<ValidationId, WorkspaceFailure> {
        if self.safe_mode {
            return Err(WorkspaceFailure::Message("safe_mode_blocked"));
        }
        if self.validation.is_some() {
            return Err(WorkspaceFailure::Message(
                "workspace_execution_validation_busy",
            ));
        }
        self.state
            .authorize_execution(ExecutionSource::ProjectLocal)?;
        if !self.database_ready || !self.identity_ready {
            self.state.set_trust(WorkspaceTrust::Untrusted);
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        let next = self.next_validation.ok_or(WorkspaceFailure::Plain(
            "workspace validation identity exhausted".into(),
        ))?;
        self.next_validation = next.checked_add(1);
        let id = ValidationId(next);
        let mut generations = HashMap::new();
        for root in self.state.roots() {
            match self
                .service
                .inspect_root(root.identity.canonical_path.clone())
            {
                Ok(generation) => {
                    generations.insert(generation, root.identity.clone());
                }
                Err(e) => {
                    self.state.set_trust(WorkspaceTrust::Untrusted);
                    return Err(WorkspaceFailure::Diagnostic(
                        "workspace_execution_validation_failed",
                        e.to_string(),
                    ));
                }
            }
        }
        self.validation = Some(Validation {
            id,
            source: self.version(),
            failed: false,
            generations,
        });
        Ok(id)
    }
    pub(in crate::editor::session) fn poll(&mut self) -> Vec<WorkspaceUpdate> {
        let mut updates = Vec::new();
        loop {
            let event = match self.service.try_event() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => {
                    self.state.set_trust(WorkspaceTrust::Untrusted);
                    self.root_operation = None;
                    if let Some(v) = self.validation.take() {
                        updates.push(WorkspaceUpdate::Validated {
                            id: v.id,
                            permit: None,
                        });
                    }
                    updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                        "workspace_trust_state_failed",
                        e.to_string(),
                    )));
                    break;
                }
            };
            self.accept_event(event, &mut updates);
        }
        updates
    }
    fn accept_event(&mut self, event: TrustEvent, updates: &mut Vec<WorkspaceUpdate>) {
        match event {
            TrustEvent::Loaded { generation, result }
                if self.load_generation == Some(generation) =>
            {
                self.load_generation = None;
                self.database_ready = true;
                match result {
                    Ok(db) => {
                        self.store.replace(db.identities);
                        self.store.replace_seen(db.seen);
                    }
                    Err(e) => {
                        self.store.replace(Vec::new());
                        self.store.replace_seen(Vec::new());
                        updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                            "workspace_trust_state_failed",
                            e.to_string(),
                        )));
                    }
                }
                self.reconcile_trust();
            }
            TrustEvent::RootInspected { generation, result }
                if self
                    .identity_generation
                    .as_ref()
                    .is_some_and(|(g, _, _)| *g == generation) =>
            {
                let (_, root, source) = self.identity_generation.take().expect("matched identity");
                if source != self.version {
                    return;
                }
                match result {
                    Ok(identity) => match self.state.replace_root_identity(root, identity) {
                        Ok(()) => {
                            self.identity_ready = true;
                            self.changed();
                        }
                        Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                    },
                    Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                }
                self.reconcile_trust();
            }
            TrustEvent::RootInspected { generation, result }
                if self
                    .root_operation
                    .as_ref()
                    .is_some_and(|r| r.generation == generation) =>
            {
                let operation = self.root_operation.take().expect("matched root request");
                if operation.source != self.version {
                    updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Message(
                        "workspace_root_stale",
                    )));
                    return;
                }
                match result {
                    Ok(identity) => {
                        let name = identity.canonical_path.file_name().map_or_else(
                            || identity.canonical_path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        match self.state.add_root(name, identity.clone()) {
                            Ok(new) => match operation.replaces {
                                Some(old) => match self.switch_root(old, new) {
                                    Ok(old) => updates.push(WorkspaceUpdate::RootSwitched {
                                        path: identity.canonical_path,
                                        old,
                                    }),
                                    Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                                },
                                None => {
                                    self.changed();
                                    updates
                                        .push(WorkspaceUpdate::RootAdded(identity.canonical_path));
                                }
                            },
                            Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                        }
                    }
                    Err(e) => updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Plain(
                        format!("{}: {e}", operation.requested_path.display()),
                    ))),
                }
            }
            TrustEvent::RootInspected { generation, result }
                if self
                    .validation
                    .as_ref()
                    .is_some_and(|v| v.generations.contains_key(&generation)) =>
            {
                let validation = self.validation.as_mut().expect("matched validation");
                let expected = validation
                    .generations
                    .remove(&generation)
                    .expect("matched generation");
                let previously_failed = validation.failed;
                validation.failed |= !result.as_ref().is_ok_and(|id| id == &expected)
                    || !self.store.is_trusted(&expected);
                if validation.failed {
                    if let Ok(actual) = &result {
                        if actual.canonical_path == expected.canonical_path && actual != &expected {
                            if let Some(root) = self
                                .state
                                .roots()
                                .iter()
                                .find(|root| root.identity == expected)
                                .map(|root| root.id)
                            {
                                let _ = self.state.replace_root_identity(root, actual.clone());
                            }
                        }
                    }
                    if !previously_failed {
                        updates.push(WorkspaceUpdate::ScopeInvalidated);
                    }
                    self.state.set_trust(WorkspaceTrust::Untrusted);
                    self.confirmation = None;
                    self.version = WorkspaceVersion(Arc::new(()));
                }
                if validation.generations.is_empty() {
                    let validation = self.validation.take().expect("completed validation");
                    let valid =
                        !validation.failed && validation.source == self.version && !self.safe_mode;
                    if !valid {
                        self.state.set_trust(WorkspaceTrust::Untrusted);
                    }
                    updates.push(WorkspaceUpdate::Validated {
                        id: validation.id,
                        permit: valid.then(|| ExecutionPermit {
                            id: validation.id,
                            source: self.version(),
                        }),
                    });
                }
            }
            TrustEvent::Persisted { generation, result }
                if self
                    .persist
                    .as_ref()
                    .is_some_and(|p| p.generation == generation) =>
            {
                let pending = self.persist.take().expect("matched persistence");
                if pending.source != self.version {
                    return;
                }
                match result {
                    Ok(()) => {
                        self.reconcile_trust();
                        if let Some(target) = pending.target {
                            updates.push(WorkspaceUpdate::Message(
                                if target == WorkspaceTrust::Trusted {
                                    "workspace_trusted"
                                } else {
                                    "workspace_untrusted"
                                },
                            ));
                        }
                    }
                    Err(e) => {
                        if pending.target == Some(WorkspaceTrust::Trusted) {
                            for root in self.state.roots() {
                                self.store.revoke(&root.identity);
                            }
                        }
                        self.state.set_trust(WorkspaceTrust::Untrusted);
                        updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                            "workspace_trust_state_failed",
                            e.to_string(),
                        )));
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
impl WorkspaceSession {
    pub(in crate::editor::session) fn fixture_identity(
        &mut self,
        root: RootId,
        identity: RootIdentity,
    ) -> Result<(), WorkspaceError> {
        self.state.replace_root_identity(root, identity)?;
        self.changed();
        self.database_ready = true;
        self.identity_ready = true;
        Ok(())
    }
    pub(in crate::editor::session) fn fixture_add_root(
        &mut self,
        name: String,
        identity: RootIdentity,
    ) -> Result<RootId, WorkspaceError> {
        let id = self.state.add_root(name, identity)?;
        self.changed();
        Ok(id)
    }
    pub(in crate::editor::session) fn fixture_trust_identity(&mut self, identity: RootIdentity) {
        assert!(self.state.roots().iter().any(|r| r.identity == identity));
        self.store.trust(identity);
    }
    pub(in crate::editor::session) fn fixture_safe_mode(&mut self, safe: bool) {
        self.safe_mode = safe;
        self.changed();
    }
    pub(in crate::editor::session) fn state_path(&self) -> &Path {
        &self.state_path
    }
    /// Test-only: whether the real trust store would trust this identity. The sandbox must not
    /// put its fixture in here, and a claim to that effect has to be observable.
    #[cfg(test)]
    pub(in crate::editor::session) fn fixture_is_trusted(
        &self,
        identity: &crate::workspace::RootIdentity,
    ) -> bool {
        self.store.is_trusted(identity)
    }
    /// Test-only: whether this identity has been SEEN by the trust store, which is a weaker
    /// state than trusted but still evidence the fixture touched the store.
    #[cfg(test)]
    pub(in crate::editor::session) fn fixture_is_seen(
        &self,
        identity: &crate::workspace::RootIdentity,
    ) -> bool {
        self.store.is_seen(identity)
    }
    pub(in crate::editor::session) fn seen(&self, identity: &RootIdentity) -> bool {
        self.store.is_seen(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (WorkspaceSession, tempfile::TempDir) {
        let state = WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [
                (
                    "one".into(),
                    RootIdentity {
                        canonical_path: "/one".into(),
                        filesystem_identity: Some((1, 1)),
                    },
                ),
                (
                    "two".into(),
                    RootIdentity {
                        canonical_path: "/two".into(),
                        filesystem_identity: Some((1, 2)),
                    },
                ),
            ],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut owner = WorkspaceSession::new(
            state,
            directory.path().join("trust.toml"),
            4,
            false,
            false,
            4,
        );
        owner.database_ready = true;
        owner.identity_ready = true;
        for root in owner.state.roots() {
            owner.store.trust(root.identity.clone());
            owner.store.mark_seen(root.identity.clone());
        }
        owner.reconcile_trust();
        (owner, directory)
    }
    #[test]
    fn confirmation_is_bound_to_identity_root_selection_cancellation_and_safe_mode() {
        let (mut owner, _directory) = fixture();
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.cancel_confirmation();
        assert!(
            owner
                .confirm()
                .is_err()
        );
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.set_active_root(RootId(2)).unwrap();
        assert!(
            owner
                .confirm()
                .is_err()
        );
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        let mut replacement = owner.roots()[0].identity.clone();
        replacement.filesystem_identity = Some((1, 99));
        owner.fixture_identity(RootId(1), replacement).unwrap();
        assert!(
            owner
                .confirm()
                .is_err()
        );
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.fixture_safe_mode(true);
        assert!(
            owner
                .confirm()
                .is_err()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while owner.persist.is_some() {
            owner.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn completed_validation_permits_reject_later_root_changes_and_foreign_owners() {
        let (mut owner, _directory) = fixture();
        let identity = owner.roots()[0].identity.clone();
        owner.validation = Some(Validation {
            id: ValidationId(3),
            source: owner.version(),
            failed: false,
            generations: HashMap::from([(7, identity.clone())]),
        });
        let mut events = Vec::new();
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 7,
                result: Ok(identity),
            },
            &mut events,
        );
        let WorkspaceUpdate::Validated {
            id,
            permit: Some(permit),
        } = events.pop().unwrap()
        else {
            panic!("permit")
        };
        owner.set_active_root(RootId(2)).unwrap();
        assert!(!owner.consume_permit(permit, id));
        let (foreign, _foreign_directory) = fixture();
        let permit = ExecutionPermit {
            id,
            source: foreign.version(),
        };
        assert!(!owner.consume_permit(permit, id));
        let permit = ExecutionPermit {
            id,
            source: owner.version(),
        };
        assert!(owner.consume_permit(permit, id));
    }
    #[test]
    fn first_identity_failure_revokes_before_other_probes_finish_and_old_results_do_not_revalidate()
    {
        let (mut owner, _directory) = fixture();
        let identity = owner.roots()[0].identity.clone();
        let before = owner.version();
        owner.validation = Some(Validation {
            id: ValidationId(4),
            source: before.clone(),
            failed: false,
            generations: HashMap::from([(7, identity.clone()), (8, identity.clone())]),
        });
        let mut changed = identity.clone();
        changed.filesystem_identity = Some((1, 77));
        let mut events = Vec::new();
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 7,
                result: Ok(changed),
            },
            &mut events,
        );
        assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
        assert_ne!(owner.version(), before);
        assert!(matches!(
            events.pop(),
            Some(WorkspaceUpdate::ScopeInvalidated)
        ));
        assert!(events.is_empty());
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 8,
                result: Ok(identity.clone()),
            },
            &mut events,
        );
        assert!(matches!(
            events.pop(),
            Some(WorkspaceUpdate::Validated { permit: None, .. })
        ));
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 8,
                result: Ok(identity),
            },
            &mut events,
        );
        assert!(events.is_empty());
        owner.next_validation = None;
        assert!(owner.begin_validation().is_err());
    }
}
