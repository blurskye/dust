//! Workspace roots and their trust. Project-defined commands run only in a trusted workspace.
//! Feature execution payloads remain with their callers, not in this owner.
mod discovery;
use crate::editor::EditorState;
use crate::workspace::ExecutionSource;
use crate::workspace::RootId;
use crate::workspace::RootIdentity;
use crate::workspace::TrustEvent;
use crate::workspace::TrustStore;
use crate::workspace::WorkspaceError;
use crate::workspace::WorkspaceRoot;
use crate::workspace::WorkspaceState;
use crate::workspace::WorkspaceTrust;
use crate::workspace::WorkspaceTrustService;

pub(crate) const TRUST_QUEUE_CAPACITY: usize = 8;

pub(in crate::app) use discovery::DiscoveryKind;

pub(in crate::app) use discovery::ExecutionLane;

pub(in crate::app) use discovery::ExecutionReady;

pub(in crate::app) use discovery::WorkspacePreflightEvent;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug)]
pub(in crate::app) struct WorkspaceVersion(Arc<()>);
impl PartialEq for WorkspaceVersion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for WorkspaceVersion {}
pub(in crate::app) enum WorkspaceFailure {
    Message(&'static str),
    Diagnostic(&'static str, String),
    Plain(String),
}
impl From<WorkspaceError> for WorkspaceFailure {
    fn from(e: WorkspaceError) -> Self {
        Self::Plain(e.to_string())
    }
}
pub(in crate::app) enum WorkspaceUpdate {
    Message(&'static str),
    Failure(WorkspaceFailure),
    RootAdded(PathBuf),
    /// The editor moved from the `old` root to the new `path`.
    RootSwitched {
        path: PathBuf,
        old: PathBuf,
    },
}
struct RootOperation {
    generation: u64,
    source: WorkspaceVersion,
    requested_path: PathBuf,
    /// The root this one replaces when switching worktrees.
    replaces: Option<RootId>,
}
struct Confirmation {
    source: WorkspaceVersion,
    identities: Vec<RootIdentity>,
}
struct Persist {
    generation: u64,
    source: WorkspaceVersion,
    target: Option<WorkspaceTrust>,
    identities: Vec<RootIdentity>,
}
pub(super) struct WorkspaceSession {
    state: WorkspaceState,
    version: WorkspaceVersion,
    service: WorkspaceTrustService,
    store: TrustStore,
    // Only a successful save may make newly granted trust effective.
    persisted_identities: Vec<RootIdentity>,
    state_path: PathBuf,
    load_generation: Option<u64>,
    identity_generation: Option<(u64, RootId, WorkspaceVersion)>,
    persist: Option<Persist>,
    database_ready: bool,
    identity_ready: bool,
    startup_prompt_checked: bool,
    startup_root_seen: Option<bool>,
    safe_mode: bool,
    probe_cancelled: Arc<AtomicBool>,
    root_operation: Option<RootOperation>,
    confirmation: Option<Confirmation>,
    language_roots: discovery::Discovery,
    debugger_roots: discovery::Discovery,
    preflights: discovery::Preflights,
}
impl WorkspaceSession {
    pub(in crate::app) fn edit_scope(&self) -> &WorkspaceState {
        &self.state
    }
    pub(in crate::app) fn new(
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
            persisted_identities: Vec::new(),
            state_path,
            load_generation,
            identity_generation,
            persist: None,
            database_ready: false,
            identity_ready: false,
            startup_prompt_checked: false,
            startup_root_seen: None,
            safe_mode,
            probe_cancelled: Arc::new(AtomicBool::new(true)),
            root_operation: None,
            confirmation: None,
            language_roots: discovery::Discovery::default(),
            debugger_roots: discovery::Discovery::default(),
            preflights: discovery::Preflights::new(preflight_capacity),
        }
    }
    pub(in crate::app) fn version(&self) -> WorkspaceVersion {
        self.version.clone()
    }
    pub(in crate::app) fn roots(&self) -> &[WorkspaceRoot] {
        self.state.roots()
    }
    pub(in crate::app) fn root_revision(&self) -> u64 {
        self.state.root_revision()
    }
    /// Every workspace root this machine has opened, for the project list.
    pub(in crate::app) fn seen_roots(&self) -> Vec<PathBuf> {
        self.store
            .seen_identities()
            .into_iter()
            .map(|identity| identity.canonical_path)
            .collect()
    }
    pub(in crate::app) fn active_root(&self) -> Option<RootId> {
        self.state.active_root()
    }
    pub(in crate::app) fn trust(&self) -> WorkspaceTrust {
        self.state.trust()
    }
    pub(in crate::app) fn safe_mode(&self) -> bool {
        self.safe_mode
    }
    pub(in crate::app) fn root_operation_pending(&self) -> bool {
        self.root_operation.is_some()
    }
    pub(in crate::app) fn pending_root_switch(&self) -> Option<(&Path, &Path)> {
        let operation = self.root_operation.as_ref()?;
        let old = self
            .roots()
            .iter()
            .find(|root| Some(root.id) == operation.replaces)?;
        Some((&old.identity.canonical_path, &operation.requested_path))
    }
    pub(in crate::app) fn cancel_root_switch(&mut self) {
        self.root_operation = None;
    }
    pub(in crate::app) fn persist_pending(&self) -> bool {
        self.service.pending_writes()
    }
    pub(in crate::app) fn trust_shutdown_result(&self) -> Result<(), String> {
        self.service.shutdown_result()
    }
    pub(in crate::app) fn require_path(
        &self,
        path: &Path,
    ) -> Result<RootId, WorkspaceError> {
        self.state.require_path(path)
    }
    pub(in crate::app) fn root_for_canonical_path(
        &self,
        path: &Path,
    ) -> Option<&WorkspaceRoot> {
        self.state.root_for_canonical_path(path)
    }
    pub(in crate::app) fn authorize_execution(
        &self,
        source: ExecutionSource,
    ) -> Result<(), WorkspaceError> {
        self.state.authorize_execution(source)
    }
    pub(in crate::app) fn probe_cancellation(&self) -> Option<Arc<AtomicBool>> {
        self.execution_allowed().ok()?;
        Some(self.probe_cancelled.clone())
    }
    fn changed(&mut self) {
        self.probe_cancelled.store(true, Ordering::Release);
        self.version = WorkspaceVersion(Arc::new(()));
        self.confirmation = None;
        self.state.set_trust(WorkspaceTrust::Untrusted);
    }
    /// Add `path` as a root, or with `replaces`, move the editor from that root to `path`.
    pub(in crate::app) fn add_root_request(
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
    /// Ignore a pending inspection when its caller abandons this destination.
    pub(in crate::app) fn cancel_root_request(&mut self, path: &Path) {
        if self
            .root_operation
            .as_ref()
            .is_some_and(|operation| operation.requested_path == path)
        {
            self.root_operation = None;
        }
    }
    /// Stage the replacement so the root limit applies to the final root set.
    fn switch_root(
        &mut self,
        old: RootId,
        name: String,
        identity: RootIdentity,
    ) -> Result<PathBuf, WorkspaceError> {
        let mut state = self.state.clone();
        let single_root = state.roots().len() == 1;
        let old_path = state
            .roots()
            .iter()
            .find(|root| root.id == old)
            .ok_or(WorkspaceError::UnknownRoot(old))?
            .identity
            .canonical_path
            .clone();
        if !single_root {
            state.remove_root(old)?;
        }
        state.add_root(name, identity)?;
        if single_root {
            state.remove_root(old)?;
        }
        self.state = state;

        self.changed();
        self.reconcile_trust();
        Ok(old_path)
    }
    pub(in crate::app) fn remove_root(
        &mut self,
        id: RootId,
    ) -> Result<WorkspaceRoot, WorkspaceError> {
        let root = self.state.remove_root(id)?;
        self.changed();
        self.reconcile_trust();
        Ok(root)
    }
    pub(in crate::app) fn set_active_root(
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
    pub(in crate::app) fn reconcile_trust(&mut self) {
        let trusted = self.database_ready
            && self.identity_ready
            && self.state.roots().iter().all(|r| {
                r.identity.filesystem_identity.is_some()
                    && self.store.is_trusted(&r.identity)
                    && self.persisted_identities.contains(&r.identity)
            });
        self.state.set_trust(if trusted {
            WorkspaceTrust::Trusted
        } else {
            WorkspaceTrust::Untrusted
        });
        if trusted && !self.safe_mode {
            if self.probe_cancelled.load(Ordering::Acquire) {
                self.probe_cancelled = Arc::new(AtomicBool::new(false));
            }
        } else {
            self.probe_cancelled.store(true, Ordering::Release);
        }
    }
    pub(in crate::app) fn should_prompt(&mut self, input_busy: bool) -> bool {
        if self.startup_prompt_checked || !self.database_ready || !self.identity_ready || input_busy
        {
            return false;
        }
        self.startup_prompt_checked = true;
        !self.safe_mode
            && self.state.trust() != WorkspaceTrust::Trusted
            && !self.startup_root_seen.unwrap_or_else(|| {
                self.state
                    .active_root()
                    .and_then(|id| self.state.roots().iter().find(|r| r.id == id))
                    .is_none_or(|r| self.store.is_seen(&r.identity))
            })
    }
    pub(in crate::app) fn cancel_confirmation(&mut self) {
        self.confirmation = None;
    }
    pub(in crate::app) fn request_confirmation(
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
            if !self.store.is_seen(identity) {
                return Err(WorkspaceFailure::Plain(
                    "trust state contains too many identities".into(),
                ));
            }
        }
        self.confirmation = Some(Confirmation {
            source: self.version(),
            identities,
        });
        if changed && let Err(error) = self.persist_state(None) {
            self.confirmation = None;
            return Err(error);
        }
        Ok(())
    }
    pub(in crate::app) fn confirm(&mut self) -> Result<(), WorkspaceFailure> {
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
    pub(in crate::app) fn revoke(&mut self) -> Result<(), WorkspaceFailure> {
        self.changed();
        for root in self.state.roots() {
            self.store.revoke(&root.identity);
        }
        if !self.database_ready {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        self.persist_state(Some(WorkspaceTrust::Untrusted))
    }
    /// Forget a folder that is going away, so the trust file doesn't collect dead entries.
    pub(in crate::app) fn forget_folder(
        &mut self,
        path: &Path,
    ) -> Result<(), WorkspaceFailure> {
        self.store.forget(path);
        // Before the trust file has loaded, writing it would drop what it holds.
        if !self.database_ready {
            return Ok(());
        }
        self.persist_state(None)
    }
    fn persist_state(&mut self, target: Option<WorkspaceTrust>) -> Result<(), WorkspaceFailure> {
        if !self.database_ready {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        let identities = self.store.identities();
        match self.service.persist(
            self.state_path.clone(),
            identities.clone(),
            self.store.seen_identities(),
            if target == Some(WorkspaceTrust::Untrusted) {
                self.state
                    .roots()
                    .iter()
                    .map(|root| root.identity.clone())
                    .collect()
            } else {
                Vec::new()
            },
        ) {
            Ok(generation) => {
                self.persist = Some(Persist {
                    generation,
                    source: self.version(),
                    target,
                    identities,
                });
                Ok(())
            }
            Err(e) => {
                self.discard_unsaved_trust();
                self.probe_cancelled.store(true, Ordering::Release);
                self.state.set_trust(WorkspaceTrust::Untrusted);
                Err(WorkspaceFailure::Diagnostic(
                    "workspace_trust_state_failed",
                    e.to_string(),
                ))
            }
        }
    }
    fn discard_unsaved_trust(&mut self) {
        for identity in self.store.identities() {
            if !self.persisted_identities.contains(&identity) {
                self.store.revoke(&identity);
            }
        }
    }
    fn record_opened_roots(&mut self) -> Result<(), WorkspaceFailure> {
        if !self.database_ready || !self.identity_ready {
            return Ok(());
        }
        if self.startup_root_seen.is_none() {
            self.startup_root_seen = Some(
                self.state
                    .active_root()
                    .and_then(|id| self.state.roots().iter().find(|r| r.id == id))
                    .is_none_or(|r| self.store.is_seen(&r.identity)),
            );
        }
        let mut changed = false;
        for root in self.state.roots() {
            changed |= self.store.mark_seen(root.identity.clone());
        }
        if changed {
            self.persist_state(None)?;
        }
        Ok(())
    }
    /// Whether project-defined commands (tasks, terminals, debuggers, language servers, tool
    /// installs) may run now. Checked when a command is chosen and again when it spawns.
    pub(in crate::app) fn execution_allowed(&self) -> Result<(), WorkspaceFailure> {
        if self.safe_mode {
            return Err(WorkspaceFailure::Message("safe_mode_blocked"));
        }
        if !self.database_ready || !self.identity_ready {
            return Err(WorkspaceFailure::Message("workspace_trust_not_ready"));
        }
        self.state
            .authorize_execution(ExecutionSource::ProjectLocal)
            .map_err(WorkspaceFailure::from)
    }
    pub(in crate::app) fn poll(
        &mut self,
        editor: &EditorState,
    ) -> Vec<WorkspaceUpdate> {
        let before = self.version();
        let mut updates = Vec::new();
        loop {
            let event = match self.service.try_event() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => {
                    self.probe_cancelled.store(true, Ordering::Release);
                    self.state.set_trust(WorkspaceTrust::Untrusted);
                    self.root_operation = None;
                    updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                        "workspace_trust_state_failed",
                        e.to_string(),
                    )));
                    break;
                }
            };
            self.accept_event(event, &mut updates, editor);
        }
        if (self.startup_root_seen.is_none() || before != self.version)
            && let Err(error) = self.record_opened_roots()
        {
            updates.push(WorkspaceUpdate::Failure(error));
        }
        updates
    }
    fn accept_event(
        &mut self,
        event: TrustEvent,
        updates: &mut Vec<WorkspaceUpdate>,
        editor: &EditorState,
    ) {
        match event {
            TrustEvent::Loaded { generation, result }
                if self.load_generation == Some(generation) =>
            {
                self.load_generation = None;
                self.database_ready = result.is_ok();
                match result {
                    Ok(db) => {
                        self.persisted_identities = db.identities.clone();
                        self.store.replace(db.identities);
                        self.store.replace_seen(db.seen);
                    }
                    Err(e) => {
                        self.persisted_identities.clear();
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
                let (_, root, _) = self.identity_generation.take().expect("matched identity");
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
                        match operation.replaces {
                            Some(old) => {
                                if let Some(root) =
                                    self.state.roots().iter().find(|root| root.id == old)
                                    && editor.buffers.values().any(|buffer| {
                                        buffer.dirty
                                            && buffer.path.as_deref().is_some_and(|file| {
                                                leaves_root(
                                                    file,
                                                    &root.identity.canonical_path,
                                                    &identity.canonical_path,
                                                )
                                            })
                                    })
                                {
                                    updates.push(WorkspaceUpdate::Failure(
                                        WorkspaceFailure::Message("workspace_root_dirty_blocker"),
                                    ));
                                    return;
                                }
                                match self.switch_root(old, name, identity.clone()) {
                                    Ok(old) => updates.push(WorkspaceUpdate::RootSwitched {
                                        path: identity.canonical_path,
                                        old,
                                    }),
                                    Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                                }
                            }
                            None => match self.state.add_root(name, identity.clone()) {
                                Ok(_) => {
                                    self.changed();
                                    self.reconcile_trust();
                                    updates
                                        .push(WorkspaceUpdate::RootAdded(identity.canonical_path));
                                }
                                Err(e) => updates.push(WorkspaceUpdate::Failure(e.into())),
                            },
                        }
                    }
                    Err(e) => updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Plain(
                        format!("{}: {e}", operation.requested_path.display()),
                    ))),
                }
            }
            TrustEvent::Persisted { generation, result }
                if self
                    .persist
                    .as_ref()
                    .is_some_and(|p| p.generation == generation) =>
            {
                let pending = self.persist.take().expect("matched persistence");
                match result {
                    Ok(()) => {
                        self.persisted_identities = pending.identities;
                        self.reconcile_trust();
                        if pending.source == self.version
                            && let Some(target) = pending.target
                        {
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
                        self.discard_unsaved_trust();
                        self.probe_cancelled.store(true, Ordering::Release);
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

/// Nested worktrees keep files inside the new root when moving down, but leave the old
/// worktree's files behind when moving up.
pub(in crate::app) fn leaves_root(file: &Path, old: &Path, new: &Path) -> bool {
    file.starts_with(old) && (!file.starts_with(new) || old.starts_with(new))
}

#[cfg(test)]
impl WorkspaceSession {
    pub(in crate::app) fn fixture_identity(
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
    pub(in crate::app) fn fixture_add_root(
        &mut self,
        name: String,
        identity: RootIdentity,
    ) -> Result<RootId, WorkspaceError> {
        let id = self.state.add_root(name, identity)?;
        self.changed();
        Ok(id)
    }
    pub(in crate::app) fn fixture_trust_identity(&mut self, identity: RootIdentity) {
        assert!(self.state.roots().iter().any(|r| r.identity == identity));
        self.persisted_identities.push(identity.clone());
        self.store.trust(identity);
    }
    pub(in crate::app) fn fixture_safe_mode(&mut self, safe: bool) {
        self.safe_mode = safe;
        self.changed();
    }
    pub(in crate::app) fn state_path(&self) -> &Path {
        &self.state_path
    }
    pub(in crate::app) fn seen(&self, identity: &RootIdentity) -> bool {
        self.store.is_seen(identity)
    }
}

#[cfg(test)]
mod tests {
    use crate::app::workspace::*;
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
        owner.persisted_identities = owner.store.identities();
        owner.reconcile_trust();
        (owner, directory)
    }
    #[test]
    fn removing_a_root_preserves_trust_of_remaining_roots() {
        let (mut owner, _directory) = fixture();
        owner.remove_root(RootId(2)).unwrap();
        assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
        assert!(owner.execution_allowed().is_ok());
    }

    #[test]
    fn workspace_root_removal_reconciles_remaining_trust() {
        let (mut owner, _directory) = fixture();
        owner.remove_root(RootId(2)).unwrap();
        assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
        assert!(owner.execution_allowed().is_ok());
    }

    #[test]
    fn workspace_root_addition_reconciles_previously_saved_trust() {
        let (mut owner, _directory) = fixture();
        let identity = RootIdentity {
            canonical_path: "/three".into(),
            filesystem_identity: Some((1, 3)),
        };
        owner.store.trust(identity.clone());
        owner.persisted_identities.push(identity.clone());
        owner
            .add_root_request(identity.canonical_path.clone(), None)
            .unwrap_or_else(|_| panic!("root request"));
        let generation = owner.root_operation.as_ref().unwrap().generation;
        let mut updates = Vec::new();
        owner.accept_event(
            TrustEvent::RootInspected {
                generation,
                result: Ok(identity),
            },
            &mut updates,
            &EditorState::new("/".into()),
        );
        assert!(matches!(
            updates.as_slice(),
            [WorkspaceUpdate::RootAdded(_)]
        ));
        assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
        assert!(owner.execution_allowed().is_ok());
    }

    #[test]
    fn workspace_root_replacement_at_capacity_is_atomic() {
        let (mut owner, _directory) = fixture();
        for index in 3..=64 {
            owner
                .state
                .add_root(
                    format!("root-{index}"),
                    RootIdentity {
                        canonical_path: format!("/root-{index}").into(),
                        filesystem_identity: Some((1, index)),
                    },
                )
                .unwrap();
        }
        let identity = RootIdentity {
            canonical_path: "/replacement".into(),
            filesystem_identity: Some((1, 100)),
        };
        owner
            .add_root_request(identity.canonical_path.clone(), Some(RootId(1)))
            .unwrap_or_else(|_| panic!("root request"));
        let generation = owner.root_operation.as_ref().unwrap().generation;
        let mut updates = Vec::new();
        owner.accept_event(
            TrustEvent::RootInspected {
                generation,
                result: Ok(identity.clone()),
            },
            &mut updates,
            &EditorState::new("/".into()),
        );
        assert!(
            matches!(updates.as_slice(), [WorkspaceUpdate::RootSwitched { path, old }]
            if path == &identity.canonical_path && old == Path::new("/one"))
        );
        assert_eq!(owner.roots().len(), 64);
        assert!(owner.roots().iter().all(|root| root.id != RootId(1)));
        let new = owner.active_root().unwrap();
        assert_eq!(
            owner
                .roots()
                .iter()
                .find(|root| root.id == new)
                .unwrap()
                .identity,
            identity
        );

        let roots = owner.roots().to_vec();
        let version = owner.version();
        let revision = owner.root_revision();
        assert!(
            owner
                .switch_root(new, "duplicate".into(), roots[0].identity.clone())
                .is_err()
        );
        assert_eq!(owner.roots(), roots);
        assert_eq!(owner.active_root(), Some(new));
        assert_eq!(owner.version(), version);
        assert_eq!(owner.root_revision(), revision);
    }

    #[test]
    fn workspace_trust_persistence_survives_active_root_changes() {
        for fail in [false, true] {
            let (mut owner, directory) = fixture();
            owner.store.replace(Vec::new());
            owner.persisted_identities.clear();
            owner.reconcile_trust();
            if fail {
                owner.state_path = directory.path().to_path_buf();
            }
            owner
                .request_confirmation()
                .unwrap_or_else(|_| panic!("prompt"));
            owner.confirm().unwrap_or_else(|_| panic!("save trust"));
            owner.set_active_root(RootId(2)).unwrap();
            assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
            assert!(owner.execution_allowed().is_err());

            let mut updates = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while owner.persist_pending() {
                updates.extend(owner.poll(&EditorState::new("/".into())));
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            if fail {
                assert!(matches!(
                    updates.as_slice(),
                    [WorkspaceUpdate::Failure(WorkspaceFailure::Diagnostic(
                        "workspace_trust_state_failed",
                        _
                    ))]
                ));
                assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
                assert!(owner.store.identities().is_empty());
                owner.reconcile_trust();
                assert!(owner.execution_allowed().is_err());
            } else {
                assert!(updates.is_empty(), "no stale trust confirmation message");
                assert_eq!(owner.trust(), WorkspaceTrust::Trusted);
                assert!(owner.execution_allowed().is_ok());
            }
        }
    }

    #[test]
    fn revoking_trust_cancels_queued_probes_permanently() {
        let (mut owner, _directory) = fixture();
        let cancelled = owner.probe_cancellation().expect("trusted");
        assert!(!cancelled.load(Ordering::Acquire));
        assert!(owner.revoke().is_ok());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(owner.probe_cancellation().is_none());
        for root in owner.state.roots() {
            owner.store.trust(root.identity.clone());
        }
        owner.persisted_identities = owner.store.identities();
        owner.reconcile_trust();
        assert!(!owner.probe_cancellation().unwrap().load(Ordering::Acquire));
        assert!(cancelled.load(Ordering::Acquire));
        owner.safe_mode = true;
        owner.reconcile_trust();
        assert!(owner.probe_cancellation().is_none());
    }
    #[test]
    fn failed_trust_persistence_cancels_queued_probes() {
        let (mut owner, _directory) = fixture();
        let cancelled = owner.probe_cancellation().expect("trusted");
        owner.persist = Some(Persist {
            generation: 7,
            source: owner.version(),
            target: None,
            identities: owner.store.identities(),
        });
        owner.accept_event(
            TrustEvent::Persisted {
                generation: 7,
                result: Err(crate::workspace::TrustStateError::UnsupportedVersion(99)),
            },
            &mut Vec::new(),
            &EditorState::new("/".into()),
        );
        assert_eq!(owner.trust(), WorkspaceTrust::Untrusted);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(owner.probe_cancellation().is_none());
    }
    #[test]
    fn cancelled_root_request_ignores_its_late_inspection() {
        let (mut owner, _directory) = fixture();
        let path = PathBuf::from("/practice");
        owner.root_operation = Some(RootOperation {
            generation: 99,
            source: owner.version(),
            requested_path: path.clone(),
            replaces: owner.active_root(),
        });
        let active = owner.active_root();
        owner.cancel_root_request(Path::new("/other"));
        assert!(owner.root_operation_pending());
        owner.cancel_root_request(&path);
        assert!(!owner.root_operation_pending());
        let mut updates = Vec::new();
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 99,
                result: Ok(RootIdentity {
                    canonical_path: path,
                    filesystem_identity: Some((1, 3)),
                }),
            },
            &mut updates,
            &EditorState::new("/".into()),
        );
        assert!(updates.is_empty());
        assert_eq!(owner.active_root(), active);
        assert_eq!(owner.roots().len(), 2);
    }

    #[test]
    fn failed_trust_load_cannot_replace_the_original_database() {
        let (mut owner, _directory) = fixture();
        std::fs::write(&owner.state_path, "broken database").unwrap();
        owner.load_generation = Some(7);
        let mut updates = Vec::new();
        owner.accept_event(
            TrustEvent::Loaded {
                generation: 7,
                result: Err(crate::workspace::TrustStateError::UnsupportedVersion(99)),
            },
            &mut updates,
            &EditorState::new("/".into()),
        );
        assert!(!owner.database_ready);
        assert!(owner.persist_state(None).is_err());
        assert_eq!(
            std::fs::read_to_string(&owner.state_path).unwrap(),
            "broken database"
        );
    }
    #[test]
    fn revoking_trust_does_not_retire_initial_root_inspection() {
        let (mut owner, _directory) = fixture();
        let root = owner.state.roots()[0].clone();
        owner.identity_ready = false;
        owner.identity_generation = Some((7, root.id, owner.version()));
        assert!(owner.revoke().is_ok());
        owner.accept_event(
            TrustEvent::RootInspected {
                generation: 7,
                result: Ok(root.identity),
            },
            &mut Vec::new(),
            &EditorState::new("/".into()),
        );
        assert!(owner.identity_ready);
    }
    #[test]
    fn confirmation_is_bound_to_identity_root_selection_cancellation_and_safe_mode() {
        let (mut owner, _directory) = fixture();
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.cancel_confirmation();
        assert!(owner.confirm().is_err());
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.set_active_root(RootId(2)).unwrap();
        assert!(owner.confirm().is_err());
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        let mut replacement = owner.roots()[0].identity.clone();
        replacement.filesystem_identity = Some((1, 99));
        owner.fixture_identity(RootId(1), replacement).unwrap();
        assert!(owner.confirm().is_err());
        owner
            .request_confirmation()
            .unwrap_or_else(|_| panic!("prompt"));
        owner.fixture_safe_mode(true);
        assert!(owner.confirm().is_err());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while owner.persist.is_some() {
            owner.poll(&EditorState::new("/".into()));
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
