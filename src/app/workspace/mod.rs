//! Workspace roots and their trust. Project-defined commands run only in a trusted workspace.
//! Feature execution payloads remain with their callers, not in this owner.
mod discovery;
use crate::editor::EditorState;
use crate::files::RootIdentity;
use crate::workspace::ExecutionSource;
use crate::workspace::RootId;
use crate::workspace::TrustEvent;
use crate::workspace::TrustStore;
use crate::workspace::WorkspaceError;
use crate::workspace::WorkspaceRoot;
use crate::workspace::WorkspaceState;
use crate::workspace::WorkspaceTrust;
use crate::workspace::WorkspaceTrustWorker;

pub(in crate::app) const TRUST_QUEUE_CAPACITY: usize = 8;

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
    request_id: u64,
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
    request_id: u64,
    source: WorkspaceVersion,
    target: Option<WorkspaceTrust>,
    identities: Vec<RootIdentity>,
}
pub(super) struct Workspace {
    state: WorkspaceState,
    version: WorkspaceVersion,
    service: WorkspaceTrustWorker,
    store: TrustStore,
    // Only a successful save may make newly granted trust effective.
    persisted_identities: Vec<RootIdentity>,
    state_path: PathBuf,
    load_request_id: Option<u64>,
    identity_request_id: Option<(u64, RootId, WorkspaceVersion)>,
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
impl Workspace {
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
        let mut service = WorkspaceTrustWorker::start(capacity);
        let version = WorkspaceVersion(Arc::new(()));
        let load_request_id = initialize
            .then(|| service.load(state_path.clone()).ok())
            .flatten();
        let identity_request_id = if initialize {
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
            load_request_id,
            identity_request_id,
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
    pub(in crate::app) fn require_path(&self, path: &Path) -> Result<RootId, WorkspaceError> {
        self.state.require_path(path)
    }
    pub(in crate::app) fn root_for_canonical_path(&self, path: &Path) -> Option<&WorkspaceRoot> {
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
    pub(in crate::app) fn changed(&mut self) {
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
        let request_id = self
            .service
            .inspect_root(path.clone())
            .map_err(|e| WorkspaceFailure::Plain(e.to_string()))?;
        self.root_operation = Some(RootOperation {
            request_id,
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
    pub(in crate::app) fn set_active_root(&mut self, id: RootId) -> Result<(), WorkspaceError> {
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
    pub(in crate::app) fn request_confirmation(&mut self) -> Result<(), WorkspaceFailure> {
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
    pub(in crate::app) fn forget_folder(&mut self, path: &Path) -> Result<(), WorkspaceFailure> {
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
            Ok(request_id) => {
                self.persist = Some(Persist {
                    request_id,
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
    pub(in crate::app) fn poll(&mut self, editor: &EditorState) -> Vec<WorkspaceUpdate> {
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
    pub(in crate::app) fn accept_event(
        &mut self,
        event: TrustEvent,
        updates: &mut Vec<WorkspaceUpdate>,
        editor: &EditorState,
    ) {
        match event {
            TrustEvent::Loaded { request_id, result }
                if self.load_request_id == Some(request_id) =>
            {
                self.load_request_id = None;
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
            TrustEvent::RootInspected { request_id, result }
                if self
                    .identity_request_id
                    .as_ref()
                    .is_some_and(|(g, _, _)| *g == request_id) =>
            {
                let (_, root, _) = self.identity_request_id.take().expect("matched identity");
                match result {
                    Ok(identity) => match self.state.replace_root_identity(root, identity) {
                        Ok(()) => {
                            self.identity_ready = true;
                            self.changed();
                        }
                        Err(e) => updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Plain(
                            e.to_string(),
                        ))),
                    },
                    Err(e) => updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Plain(
                        e.to_string(),
                    ))),
                }
                self.reconcile_trust();
            }
            TrustEvent::RootInspected { request_id, result }
                if self
                    .root_operation
                    .as_ref()
                    .is_some_and(|r| r.request_id == request_id) =>
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
                                    Err(e) => updates.push(WorkspaceUpdate::Failure(
                                        WorkspaceFailure::Plain(e.to_string()),
                                    )),
                                }
                            }
                            None => match self.state.add_root(name, identity.clone()) {
                                Ok(_) => {
                                    self.changed();
                                    self.reconcile_trust();
                                    updates
                                        .push(WorkspaceUpdate::RootAdded(identity.canonical_path));
                                }
                                Err(e) => updates.push(WorkspaceUpdate::Failure(
                                    WorkspaceFailure::Plain(e.to_string()),
                                )),
                            },
                        }
                    }
                    Err(e) => updates.push(WorkspaceUpdate::Failure(WorkspaceFailure::Plain(
                        format!("{}: {e}", operation.requested_path.display()),
                    ))),
                }
            }
            TrustEvent::Persisted { request_id, result }
                if self
                    .persist
                    .as_ref()
                    .is_some_and(|p| p.request_id == request_id) =>
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
impl Workspace {
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

pub(in crate::app) mod edits;

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::TrustedAction;
use crate::app::tasks::AdapterContext;
use crate::app::workspace::WorkspacePreflightEvent as ExecutionPreflightEvent;
use crate::tools::RegistryRequest;
use crate::tools::ToolStatus;
use crate::ui::PromptKind;
use crate::workspace::DiagnosticOwner;
use std::collections::HashSet;

impl App {
    pub(in crate::app) fn pending_preflight_count(&self) -> usize {
        self.tasks.preflight_count()
            + self.terminal_session.preflight_count()
            + self.language.preflight_count()
    }
}

impl App {
    pub(in crate::app) fn process_execution_preflight(&mut self) {
        let events = self.workspace.preflight_events();
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(ready) => {
                    if self
                        .debugger_session
                        .owns_debug_preflight(ready.result().id)
                    {
                        let id = ready.result().id;
                        if let Err(error) = self.debugger_session.debug_preflight(ready) {
                            self.debugger_session.fail_debug_preflight(id);
                            self.show_error(error);
                        }
                        self.advance_debug_launch();
                        continue;
                    }
                    if self.language.owns_preflight(ready.result().id) {
                        match self.language.finish_preflight(
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                            &self.configuration.settings().lsp,
                        ) {
                            Ok(Some(key)) => {
                                for prefix in ["lsp.", "lsp-pull:"] {
                                    if let Some(producer) =
                                        self.language.diagnostic_producer(&key, prefix)
                                    {
                                        self.diagnostics.activate(producer);
                                    }
                                }
                                self.sync_ui_diagnostics();
                            }
                            Ok(None) => {}
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    let result = ready.result();
                    let id = result.id;
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        if let Some(step) = self.tasks.take_preflight(id) {
                            self.tasks.reject_step(&step);
                        }
                        if let Some(key) = self.terminal_session.take_preflight(id) {
                            if let Some(id) = self.terminal_session.resolve(&key)
                                && let Some(reply) = self.debugger_session.take_terminal(&id)
                            {
                                let _ =
                                    reply.respond(false, Some("terminal workspace changed".into()));
                            }
                            self.terminal_session.reject(&key);
                            self.sync_terminal_panel();
                        }
                        continue;
                    }
                    if let Some(step) = self.tasks.take_preflight(id) {
                        match self.tasks.launch(
                            step,
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                        ) {
                            Ok(source) => {
                                if let Some(source) = source {
                                    self.diagnostics.activate(source);
                                    self.sync_ui_diagnostics();
                                }
                            }
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        let id = self.terminal_session.resolve(&key);
                        if id.is_some_and(|id| !self.debugger_session.terminal_current(&id)) {
                            self.terminal_session.reject(&key);
                            if let Some(id) = id {
                                self.debugger_session.take_terminal(&id);
                            }
                            continue;
                        }
                        if let Err(error) =
                            self.terminal_session
                                .launch(key.clone(), ready, &self.workspace)
                        {
                            self.terminal_session.reject(&key);
                            if let Some(reply) =
                                id.and_then(|id| self.debugger_session.take_terminal(&id))
                            {
                                let _ = reply.respond(false, Some(error.clone()));
                            }
                            self.show_error(error);
                        }
                        self.sync_terminal_panel();
                        continue;
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    if self.debugger_session.owns_debug_preflight(id) {
                        self.debugger_session.fail_debug_preflight(id);
                        self.show_error(message);
                        continue;
                    }
                    if self.language.fail_preflight(id) {
                        self.show_error(message);
                        continue;
                    }
                    let mut owned = false;
                    if let Some(step) = self.tasks.take_preflight(id) {
                        self.tasks.reject_step(&step);
                        owned = true;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        if let Some(id) = self.terminal_session.resolve(&key)
                            && let Some(reply) = self.debugger_session.take_terminal(&id)
                        {
                            let _ = reply.respond(false, Some(message.clone()));
                        }
                        self.terminal_session.reject(&key);
                        self.sync_terminal_panel();
                        owned = true;
                    }
                    if owned && root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn add_workspace_root(&mut self, path: PathBuf) {
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.workspace.add_root_request(requested_path, None) {
            Ok(()) => self.notify_message(self.message("workspace_root_loading", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn switch_workspace_root(&mut self, path: PathBuf) {
        let current = self.workspace.active_root().and_then(|id| {
            self.workspace
                .roots()
                .iter()
                .find(|root| root.id == id)
                .map(|root| (id, root.identity.canonical_path.clone()))
        });
        let Some((id, current_path)) = current else {
            self.add_workspace_root(path);
            return;
        };
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        if target == current_path {
            self.close_picker();
            self.notify_message(format!("already in {}", target.display()), false);
            return;
        }
        // A folder that is already a workspace root is made active, not opened in place of this one.
        if let Some(existing) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == target)
            .map(|root| root.id)
        {
            self.close_picker();
            self.activate_workspace_root(existing);
            return;
        }
        let unsaved = self
            .state
            .buffers
            .values()
            .filter(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_ref()
                        .is_some_and(|file| leaves_root(file, &current_path, &target))
            })
            .count();
        if unsaved > 0 {
            self.show_error(format!(
                "{unsaved} file(s) in {} have unsaved changes; save or close them first",
                current_path.file_name().map_or_else(
                    || current_path.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ));
            return;
        }
        // Terminals belong to the folder they were opened in, so switching ends them, as
        // opening another folder does in VS Code; it takes asking twice when any are open.
        let terminals = self.terminal_session.running_in_root(&current_path);
        if terminals > 0 && self.switch_ends_terminals.as_ref() != Some(&target) {
            self.switch_ends_terminals = Some(target);
            self.show_error(format!(
                "switching ends {terminals} open terminal(s) and what runs in them; switch again \
                 to go ahead"
            ));
            return;
        }
        self.switch_ends_terminals = None;
        self.close_picker();
        match self.workspace.add_root_request(target, Some(id)) {
            Ok(()) => {
                self.switch_approved_terminals = self.terminal_session.ids_in_root(&current_path);
                self.notify_message("switching…".to_owned(), false);
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    /// After a worktree switch: close the old root's clean buffers, move the editor's root and
    /// reopen the file that was in front at the same place in the new worktree.
    pub(in crate::app) fn finish_root_switch(&mut self, path: PathBuf, old: &Path) {
        self.terminal_session.close_root(old);
        let leaving = |file: &Path| leaves_root(file, old, &path);
        let reopen = self.current_persisted_location().and_then(|mut location| {
            let relative = location.path.strip_prefix(old).ok()?;
            if !leaving(&location.path) {
                return None;
            }
            location.path = path.join(relative);
            location.path.is_file().then_some(location)
        });
        // Buffers in no window go first, or closing a window's buffer would put one of them on
        // screen, and a later save would land in the old checkout.
        let hidden = self
            .state
            .buffers
            .iter()
            .filter(|(id, buffer)| {
                !buffer.dirty
                    && buffer.path.as_deref().is_some_and(leaving)
                    && !self.state.views.values().any(|view| view.buffer == *id)
            })
            .map(|(id, buffer)| (id, buffer.path.clone()))
            .collect::<Vec<_>>();
        for (buffer, closed_path) in hidden {
            let mut candidate = self.state.draft();
            candidate.remove_hidden_document(buffer);
            match self.commit_editor_draft(candidate, None, false) {
                Ok(()) => self.finish_buffer_close(buffer, closed_path.as_deref(), true),
                Err(error) => self.show_error(error),
            }
        }
        // Finish one buffer before choosing the next: otherwise fallback buffers can alternate
        // forever when several panes show the same leaving buffer.
        let leaving_buffers = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| !buffer.dirty && buffer.path.as_deref().is_some_and(leaving))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for buffer in leaving_buffers {
            let views = self
                .state
                .views
                .iter()
                .filter(|(_, state)| state.buffer == buffer)
                .map(|(view, _)| view)
                .collect::<Vec<_>>();
            for view in views {
                if let Err(error) = self.state.focus_view(view) {
                    self.show_error(error);
                    break;
                }
                self.close_buffer_target(view, buffer);
            }
        }
        // Windows left with nothing to show go too, down to the one the file reopens in.
        let front = self.state.active_view();
        let empty = self
            .state
            .views
            .iter()
            .filter(|(view, state)| {
                Some(*view) != front
                    && self.state.buffers.get(state.buffer).is_some_and(|buffer| {
                        buffer.path.is_none() && !buffer.dirty && buffer.text.len_bytes() == 0
                    })
                    && Some(state.buffer) != self.state.kept_buffer
            })
            .map(|(view, state)| (view, state.buffer))
            .collect::<Vec<_>>();
        for (view, buffer) in empty {
            if self.state.close_view(view).is_ok() {
                let mut candidate = self.state.draft();
                candidate.remove_hidden_document(buffer);
                if let Err(error) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(error);
                }
            }
        }
        if let Some(front) = front {
            let _ = self.state.focus_view(front);
        }
        self.state.set_workspace_root(path.clone());
        self.invalidate_root_scoped_operations();
        self.start_git_status_refresh_silent();
        if let Some(location) = reopen {
            self.navigate_persisted_location(location);
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.notify_message(format!("switched to {name}"), false);
    }
}

impl App {
    pub(in crate::app) fn root_removal_blocker(
        &self,
        root: crate::workspace::RootId,
    ) -> Option<String> {
        let root = self
            .workspace
            .roots()
            .iter()
            .find(|candidate| candidate.id == root)?;
        if self.state.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root.identity.canonical_path))
        }) {
            return Some(self.message("workspace_root_dirty_blocker", &[]));
        }
        if self
            .terminal_session
            .belongs_to_root(&root.identity.canonical_path)
        {
            return Some("Close the root’s terminals before removing it".into());
        }
        let busy = self.workspace.root_operation_pending()
            || self.language.active().is_some()
            || self
                .language
                .sessions()
                .values()
                .any(|state| state.restart_due.is_some())
            || self.debugger_session.is_running()
            || self
                .tooling
                .entries()
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing { .. }))
            || self.search.pending()
            || self.workspace_edits.preparing()
            || self.workspace_edits.proposal().is_some()
            || self.documents.legacy_pending()
            || self.workspace.persist_pending()
            || self.documents.has_startup_work()
            || self.documents.file_work_pending();
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }
}

impl App {
    pub(in crate::app) fn invalidate_root_scoped_operations(&mut self) {
        self.documents.cancel_startup();
        self.documents
            .cancel_old_workspace_requests(&self.workspace.version());
        self.clear_git_status();
        self.terminal_session.retain_workspace(&self.workspace);
        self.reject_terminal_responses("workspace changed");
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.language.clear_active();

                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.language.forget_launches();
        for (plugin, load) in self.extension.cancel_pending_packages() {
            self.remove_plugin_contributions(&plugin, load);
        }
        self.debugger_session.reset();
        self.input.clear_snippet();
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.tasks.cancel_workspace_tasks();
        self.clear_git_status();
        self.search.cancel();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.cancel_workspace_resources("workspace changed");
        self.discard_stale_plugin_edits();
        self.cancel_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.set_explorer_focus(false);
        self.reconcile_explorer_watch();
        self.close_picker();
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn remove_workspace_root(&mut self, id: crate::workspace::RootId) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                if let Some(active) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.state
                        .set_workspace_root(active.identity.canonical_path.clone());
                }
                self.invalidate_root_scoped_operations();
                self.notify_message(
                    self.message(
                        "workspace_root_removed",
                        &[("path", root.identity.canonical_path.display().to_string())],
                    ),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn activate_workspace_root(&mut self, id: crate::workspace::RootId) {
        let before = self.workspace.version();
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                if before != self.workspace.version() {
                    self.documents.cancel_startup();
                }
                self.documents
                    .cancel_old_workspace_requests(&self.workspace.version());
                self.clear_git_status();
                let root = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.clone());
                // The statusline and relative labels name the active root.
                if let Some(root) = &root {
                    self.state.set_workspace_root(root.clone());
                }
                let path = root
                    .map(|root| root.display().to_string())
                    .unwrap_or_default();
                self.notify_message(
                    self.message("workspace_root_activated", &[("path", path)]),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn show_workspace_failure(&mut self, failure: WorkspaceFailure) {
        let message = match failure {
            WorkspaceFailure::Message(key) => self.message(key, &[]),
            WorkspaceFailure::Diagnostic(key, diagnostic) => {
                self.message(key, &[("diagnostic", diagnostic)])
            }
            WorkspaceFailure::Plain(message) => message,
        };
        self.show_error(message);
    }
}

impl App {
    /// Run a project-defined command if the workspace is trusted.
    pub(in crate::app) fn run_trusted(&mut self, action: TrustedAction) {
        let access = match &action {
            TrustedAction::InstallRegistry { .. }
            | TrustedAction::PrepareRegistryArtifact
            | TrustedAction::InstallPackage { .. } => crate::editor::CommandAccess::HostOnly,
            _ => crate::editor::CommandAccess::Process,
        };
        if !self.require_effect_access(access) {
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        match action {
            TrustedAction::StartDebug(intent) => self.authorize_debug_launch(intent),
            TrustedAction::InstallRegistry {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                true,
            ),
            TrustedAction::PrepareRegistryArtifact => {
                if let Err(error) = self.extension.prepare_artifact(self.plugin_limits()) {
                    self.show_error(error);
                }
            }
            TrustedAction::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => self.install_package_after_validation(
                package_manifest,
                plugin_manifest,
                component,
                update,
            ),
            // A task with a matcher reports what it matched in Problems, as a VS Code task does.
            TrustedAction::RunTask(name) => {
                let adapter = match self.configuration.settings().tasks.get(&name) {
                    Some(task) => task
                        .matcher
                        .clone()
                        .map_or(AdapterContext::Plain, |matcher| AdapterContext::Linter {
                            matcher,
                            lint: task.kind == crate::config::TaskKind::Linter,
                        }),
                    None => AdapterContext::Plain,
                };
                self.start_task_plan(&name, adapter)
            }
            TrustedAction::OpenTerminal(intent) => self.open_terminal_with_response(intent, None),
            TrustedAction::OpenDapTerminal { intent, reply } => {
                self.open_terminal_with_response(intent, Some(reply))
            }
            TrustedAction::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            TrustedAction::RunLinter { name, matcher } => self.start_task_plan(
                &name,
                AdapterContext::Linter {
                    matcher,
                    lint: true,
                },
            ),
            TrustedAction::RunFormatter { name, source } => {
                self.start_task_plan(&name, AdapterContext::Formatter { source })
            }
        }
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn reconcile_workspace_trust(&mut self) {
        self.workspace.reconcile_trust();
    }
}

impl App {
    pub(in crate::app) fn process_trust_events(&mut self) {
        if let Some((old, new)) = self.workspace.pending_root_switch() {
            let dirty = self.state.buffers.values().any(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_deref()
                        .is_some_and(|file| leaves_root(file, old, new))
            });
            let new_terminal = self
                .terminal_session
                .ids_in_root(old)
                .iter()
                .any(|id| !self.switch_approved_terminals.contains(id));
            if dirty || new_terminal {
                self.workspace.cancel_root_switch();
                self.switch_approved_terminals.clear();
                self.show_error(if dirty {
                    "Workspace switch cancelled: save or close the unsaved files first"
                } else {
                    "Workspace switch cancelled: new terminals opened; switch again to confirm"
                });
            }
        }
        let before = self.workspace.version();
        let updates = self.workspace.poll(&self.state);
        if before != self.workspace.version() {
            self.documents
                .cancel_old_workspace_requests(&self.workspace.version());
        }
        for event in updates {
            match event {
                WorkspaceUpdate::Failure(e) => self.show_workspace_failure(e),
                WorkspaceUpdate::Message(key) => self.notify_message(self.message(key, &[]), false),
                WorkspaceUpdate::RootAdded(path) => {
                    self.invalidate_root_scoped_operations();
                    // A new root becomes the active one; the statusline and relative paths follow.
                    self.clear_git_status();
                    self.state.set_workspace_root(path.clone());
                    self.notify_message(
                        self.message(
                            "workspace_root_added",
                            &[("path", path.display().to_string())],
                        ),
                        false,
                    );
                }
                WorkspaceUpdate::RootSwitched { path, old } => self.finish_root_switch(path, &old),
            }
        }
    }
}

impl App {
    pub(in crate::app) fn maybe_prompt_for_unseen_workspace(&mut self) {
        // Never open under the user's fingers: keys typed a moment ago would answer it.
        let typing = self
            .last_key_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(800));
        if self.workspace.should_prompt(
            typing
                || self.input.prompt().is_some()
                || self.ui.picker.is_some()
                || self.ui.tool_menu.is_some()
                || self.ui.settings.is_some(),
        ) {
            self.request_workspace_trust();
        }
    }
}

impl App {
    pub(in crate::app) fn request_workspace_trust(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        match self.workspace.request_confirmation() {
            Ok(()) => {
                self.clear_notice();
                self.open_confirmation_actions(
                    PromptKind::WorkspaceTrust,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .workspace_trust_title
                        .clone(),
                    self.message("workspace_trust_confirmation", &[]),
                    crate::ui::ConfirmationChoice::Yes,
                    "Trust",
                    "Not now",
                );
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn confirm_workspace_trust(&mut self) {
        match self.workspace.confirm() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn revoke_workspace_trust(&mut self) {
        match self.workspace.revoke() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn workspace_relative_label(&self, path: &std::path::Path) -> String {
        self.workspace
            .roots()
            .iter()
            .find_map(|root| path.strip_prefix(&root.identity.canonical_path).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
