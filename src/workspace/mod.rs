#![forbid(unsafe_code)]

pub mod git;
pub mod search_workflow;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::document::{BufferRevision, ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};

const ABSOLUTE_ROOT_LIMIT: usize = 64;
const ABSOLUTE_DIAGNOSTIC_LIMIT: usize = 100_000;
const ABSOLUTE_PICKER_ITEM_LIMIT: usize = 100_000;
const ABSOLUTE_PROPOSAL_FILE_LIMIT: usize = 1_024;
const ABSOLUTE_PROPOSAL_EDIT_LIMIT: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RootId(pub u64);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootIdentity {
    pub canonical_path: PathBuf,
    pub filesystem_identity: Option<(u64, u64)>,
}

impl RootIdentity {
    pub fn capture(path: &Path) -> Result<Self, WorkspaceError> {
        let canonical_path = fs::canonicalize(path).map_err(|source| WorkspaceError::Io {
            operation: "canonicalize workspace root",
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&canonical_path).map_err(|source| WorkspaceError::Io {
            operation: "inspect workspace root",
            path: canonical_path.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(WorkspaceError::RootNotDirectory(canonical_path));
        }
        #[cfg(unix)]
        let filesystem_identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let filesystem_identity = None;
        Ok(Self {
            canonical_path,
            filesystem_identity,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRoot {
    pub id: RootId,
    pub name: String,
    pub identity: RootIdentity,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceTrust {
    #[default]
    Untrusted,
    Trusted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionSource {
    SystemTool,
    UserConfigured,
    ProjectLocal,
    WorkspaceTask,
    ToolInstall,
    DebugProgram,
    PluginCapability,
}

#[derive(Clone, Debug)]
pub struct WorkspaceState {
    pub id: WorkspaceId,
    roots: Vec<WorkspaceRoot>,
    active_root: Option<RootId>,
    trust: WorkspaceTrust,
    next_root_id: u64,
    root_revision: u64,
}

impl WorkspaceState {
    pub fn new(
        id: WorkspaceId,
        roots: impl IntoIterator<Item = (String, RootIdentity)>,
    ) -> Result<Self, WorkspaceError> {
        let roots = roots
            .into_iter()
            .take(ABSOLUTE_ROOT_LIMIT.saturating_add(1))
            .enumerate()
            .map(|(index, (name, identity))| WorkspaceRoot {
                id: RootId(u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1)),
                name,
                identity,
            })
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(WorkspaceError::NoRoots);
        }
        if roots.len() > ABSOLUTE_ROOT_LIMIT {
            return Err(WorkspaceError::TooManyRoots(ABSOLUTE_ROOT_LIMIT));
        }
        let mut identities = HashSet::new();
        let mut paths = HashSet::new();
        for root in &roots {
            if root.name.trim().is_empty() {
                return Err(WorkspaceError::EmptyRootName);
            }
            if !paths.insert(root.identity.canonical_path.clone())
                || !identities.insert(root.identity.clone())
            {
                return Err(WorkspaceError::DuplicateRoot(
                    root.identity.canonical_path.clone(),
                ));
            }
        }
        let next_root_id = u64::try_from(roots.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        Ok(Self {
            id,
            active_root: roots.first().map(|root| root.id),
            roots,
            trust: WorkspaceTrust::Untrusted,
            next_root_id,
            root_revision: 0,
        })
    }

    #[must_use]
    pub fn roots(&self) -> &[WorkspaceRoot] {
        &self.roots
    }

    #[must_use]
    pub const fn root_revision(&self) -> u64 {
        self.root_revision
    }

    #[must_use]
    pub const fn trust(&self) -> WorkspaceTrust {
        self.trust
    }

    pub fn set_trust(&mut self, trust: WorkspaceTrust) {
        self.trust = trust;
    }

    #[must_use]
    pub const fn active_root(&self) -> Option<RootId> {
        self.active_root
    }

    pub fn replace_root_identity(
        &mut self,
        root: RootId,
        identity: RootIdentity,
    ) -> Result<(), WorkspaceError> {
        if self
            .roots
            .iter()
            .any(|candidate| candidate.id != root && candidate.identity == identity)
        {
            return Err(WorkspaceError::DuplicateRoot(identity.canonical_path));
        }
        let candidate = self
            .roots
            .iter_mut()
            .find(|candidate| candidate.id == root)
            .ok_or(WorkspaceError::UnknownRoot(root))?;
        candidate.identity = identity;
        self.trust = WorkspaceTrust::Untrusted;
        Ok(())
    }

    pub fn add_root(
        &mut self,
        name: String,
        identity: RootIdentity,
    ) -> Result<RootId, WorkspaceError> {
        if name.trim().is_empty() {
            return Err(WorkspaceError::EmptyRootName);
        }
        if self.roots.len() >= ABSOLUTE_ROOT_LIMIT {
            return Err(WorkspaceError::TooManyRoots(ABSOLUTE_ROOT_LIMIT));
        }
        if self
            .roots
            .iter()
            .any(|root| root.identity.canonical_path == identity.canonical_path)
        {
            return Err(WorkspaceError::DuplicateRoot(identity.canonical_path));
        }
        let id = RootId(self.next_root_id);
        self.next_root_id = self.next_root_id.saturating_add(1);
        self.roots.push(WorkspaceRoot { id, name, identity });
        self.active_root = Some(id);
        self.root_revision = self.root_revision.saturating_add(1);
        self.trust = WorkspaceTrust::Untrusted;
        Ok(id)
    }

    pub fn remove_root(&mut self, root: RootId) -> Result<WorkspaceRoot, WorkspaceError> {
        if self.roots.len() == 1 {
            return Err(WorkspaceError::LastRoot);
        }
        let index = self
            .roots
            .iter()
            .position(|candidate| candidate.id == root)
            .ok_or(WorkspaceError::UnknownRoot(root))?;
        let removed = self.roots.remove(index);
        if self.active_root == Some(root) {
            self.active_root = self.roots.first().map(|candidate| candidate.id);
        }
        self.root_revision = self.root_revision.saturating_add(1);
        self.trust = WorkspaceTrust::Untrusted;
        Ok(removed)
    }

    pub fn set_active_root(&mut self, root: RootId) -> Result<(), WorkspaceError> {
        if !self.roots.iter().any(|candidate| candidate.id == root) {
            return Err(WorkspaceError::UnknownRoot(root));
        }
        self.active_root = Some(root);
        Ok(())
    }

    #[must_use]
    pub fn root_for_canonical_path(&self, path: &Path) -> Option<&WorkspaceRoot> {
        self.roots
            .iter()
            .filter(|root| path.starts_with(&root.identity.canonical_path))
            .max_by_key(|root| root.identity.canonical_path.components().count())
    }

    pub fn require_path(&self, canonical_path: &Path) -> Result<RootId, WorkspaceError> {
        if canonical_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
            return Err(WorkspaceError::PathOutsideRoots(
                canonical_path.to_path_buf(),
            ));
        }
        self.root_for_canonical_path(canonical_path)
            .map(|root| root.id)
            .ok_or_else(|| WorkspaceError::PathOutsideRoots(canonical_path.to_path_buf()))
    }

    pub fn authorize_execution(&self, source: ExecutionSource) -> Result<(), WorkspaceError> {
        let requires_trust = matches!(
            source,
            ExecutionSource::ProjectLocal
                | ExecutionSource::WorkspaceTask
                | ExecutionSource::ToolInstall
                | ExecutionSource::DebugProgram
                | ExecutionSource::PluginCapability
        );
        if requires_trust && self.trust != WorkspaceTrust::Trusted {
            return Err(WorkspaceError::TrustRequired(source));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    trusted: HashSet<RootIdentity>,
}

impl TrustStore {
    pub fn replace(&mut self, identities: impl IntoIterator<Item = RootIdentity>) {
        self.trusted = identities.into_iter().collect();
    }

    #[must_use]
    pub fn identities(&self) -> Vec<RootIdentity> {
        let mut identities = self.trusted.iter().cloned().collect::<Vec<_>>();
        identities.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        identities
    }

    pub fn trust(&mut self, identity: RootIdentity) {
        self.trusted
            .retain(|candidate| candidate.canonical_path != identity.canonical_path);
        self.trusted.insert(identity);
    }

    pub fn revoke(&mut self, identity: &RootIdentity) {
        self.trusted.remove(identity);
    }

    #[must_use]
    pub fn is_trusted(&self, identity: &RootIdentity) -> bool {
        self.trusted.contains(identity)
    }
}

pub const TRUST_STATE_VERSION: u32 = 1;
const TRUST_STATE_BYTE_LIMIT: u64 = 1024 * 1024;
const TRUST_IDENTITY_LIMIT: usize = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustDatabase {
    pub version: u32,
    pub identities: Vec<RootIdentity>,
}

impl Default for TrustDatabase {
    fn default() -> Self {
        Self {
            version: TRUST_STATE_VERSION,
            identities: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TrustStateError {
    #[error("failed to {operation} trust state `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("trust state `{}` exceeds the {} byte safety ceiling", .0.display(), TRUST_STATE_BYTE_LIMIT)]
    TooLarge(PathBuf),
    #[error("invalid trust state `{}`: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
    #[error("unsupported trust state version {0}")]
    UnsupportedVersion(u32),
    #[error("trust state contains too many identities")]
    TooManyIdentities,
    #[error("persisted trust identity lacks filesystem identity: `{}`", .0.display())]
    MissingFilesystemIdentity(PathBuf),
    #[error("workspace trust worker queue is full")]
    QueueFull,
    #[error("workspace trust worker stopped")]
    WorkerStopped,
}

fn validate_trust_database(path: &Path, database: &TrustDatabase) -> Result<(), TrustStateError> {
    if database.version != TRUST_STATE_VERSION {
        return Err(TrustStateError::UnsupportedVersion(database.version));
    }
    if database.identities.len() > TRUST_IDENTITY_LIMIT {
        return Err(TrustStateError::TooManyIdentities);
    }
    let mut unique = HashSet::new();
    let mut paths = HashSet::new();
    for identity in &database.identities {
        if identity.filesystem_identity.is_none() {
            return Err(TrustStateError::MissingFilesystemIdentity(
                identity.canonical_path.clone(),
            ));
        }
        if !identity.canonical_path.is_absolute()
            || !unique.insert(identity.clone())
            || !paths.insert(identity.canonical_path.clone())
        {
            return Err(TrustStateError::Invalid {
                path: path.to_path_buf(),
                reason: "identities must be unique absolute canonical paths".into(),
            });
        }
    }
    Ok(())
}

fn load_trust_database(path: &Path) -> Result<TrustDatabase, TrustStateError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TrustDatabase::default());
        }
        Err(source) => {
            return Err(TrustStateError::Io {
                operation: "open",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take(TRUST_STATE_BYTE_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| TrustStateError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > TRUST_STATE_BYTE_LIMIT {
        return Err(TrustStateError::TooLarge(path.to_path_buf()));
    }
    let source = String::from_utf8(bytes).map_err(|error| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let database =
        toml::from_str::<TrustDatabase>(&source).map_err(|error| TrustStateError::Invalid {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    validate_trust_database(path, &database)?;
    Ok(database)
}

fn persist_trust_database(path: &Path, database: &TrustDatabase) -> Result<(), TrustStateError> {
    validate_trust_database(path, database)?;
    let source = toml::to_string(database).map_err(|error| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if source.len() as u64 > TRUST_STATE_BYTE_LIMIT {
        return Err(TrustStateError::TooLarge(path.to_path_buf()));
    }
    let parent = path.parent().ok_or_else(|| TrustStateError::Invalid {
        path: path.to_path_buf(),
        reason: "trust state path has no parent".into(),
    })?;
    fs::create_dir_all(parent).map_err(|source| TrustStateError::Io {
        operation: "create parent directory for",
        path: path.to_path_buf(),
        source,
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| TrustStateError::Io {
        operation: "create temporary",
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(source.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| TrustStateError::Io {
            operation: "write",
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| TrustStateError::Io {
            operation: "replace",
            path: path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

#[derive(Debug)]
enum TrustRequest {
    Load {
        generation: u64,
        path: PathBuf,
    },
    InspectRoot {
        generation: u64,
        path: PathBuf,
    },
    Persist {
        generation: u64,
        path: PathBuf,
        database: TrustDatabase,
    },
}

#[derive(Debug)]
pub enum TrustEvent {
    Loaded {
        generation: u64,
        result: Result<TrustDatabase, TrustStateError>,
    },
    RootInspected {
        generation: u64,
        result: Result<RootIdentity, WorkspaceError>,
    },
    Persisted {
        generation: u64,
        result: Result<(), TrustStateError>,
    },
}

pub struct WorkspaceTrustService {
    sender: SyncSender<TrustRequest>,
    receiver: Receiver<TrustEvent>,
    next_generation: u64,
}

impl WorkspaceTrustService {
    pub fn start(queue_capacity: usize) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let (event_sender, event_receiver) = mpsc::sync_channel(queue_capacity.max(1));
        thread::Builder::new()
            .name("dust-workspace-trust".into())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    let event = match request {
                        TrustRequest::Load { generation, path } => TrustEvent::Loaded {
                            generation,
                            result: load_trust_database(&path),
                        },
                        TrustRequest::InspectRoot { generation, path } => {
                            TrustEvent::RootInspected {
                                generation,
                                result: RootIdentity::capture(&path),
                            }
                        }
                        TrustRequest::Persist {
                            generation,
                            path,
                            database,
                        } => TrustEvent::Persisted {
                            generation,
                            result: persist_trust_database(&path, &database),
                        },
                    };
                    if event_sender.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("workspace trust worker thread");
        Self {
            sender: request_sender,
            receiver: event_receiver,
            next_generation: 0,
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.saturating_add(1);
        self.next_generation
    }

    pub fn load(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let generation = self.next_generation();
        self.sender
            .try_send(TrustRequest::Load { generation, path })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn inspect_root(&mut self, path: PathBuf) -> Result<u64, TrustStateError> {
        let generation = self.next_generation();
        self.sender
            .try_send(TrustRequest::InspectRoot { generation, path })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn persist(
        &mut self,
        path: PathBuf,
        identities: Vec<RootIdentity>,
    ) -> Result<u64, TrustStateError> {
        let generation = self.next_generation();
        self.sender
            .try_send(TrustRequest::Persist {
                generation,
                path,
                database: TrustDatabase {
                    version: TRUST_STATE_VERSION,
                    identities,
                },
            })
            .map_err(map_trust_send_error)?;
        Ok(generation)
    }

    pub fn try_event(&self) -> Result<Option<TrustEvent>, TrustStateError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TrustStateError::WorkerStopped),
        }
    }
}

fn map_trust_send_error(error: TrySendError<TrustRequest>) -> TrustStateError {
    match error {
        TrySendError::Full(_) => TrustStateError::QueueFull,
        TrySendError::Disconnected(_) => TrustStateError::WorkerStopped,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExplorerNodeId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorerKind {
    Root,
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorerLoadState {
    Unloaded,
    Loading(u64),
    Loaded,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerChild {
    pub path: PathBuf,
    pub name: String,
    pub kind: ExplorerKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerNode {
    pub id: ExplorerNodeId,
    pub parent: Option<ExplorerNodeId>,
    pub root: RootId,
    pub path: PathBuf,
    pub name: String,
    pub kind: ExplorerKind,
    pub load: ExplorerLoadState,
    pub expanded: bool,
    pub children: Vec<ExplorerNodeId>,
}

#[derive(Clone, Debug, Default)]
pub struct ExplorerState {
    nodes: BTreeMap<ExplorerNodeId, ExplorerNode>,
    by_path: HashMap<PathBuf, ExplorerNodeId>,
    roots: Vec<ExplorerNodeId>,
    selected: Option<ExplorerNodeId>,
    visible_nodes: Vec<ExplorerNodeId>,
    visible_positions: HashMap<ExplorerNodeId, usize>,
    projection_generation: u64,
    next_id: u64,
    pub root_revision: u64,
}

impl ExplorerState {
    pub fn reset_roots(&mut self, roots: &[WorkspaceRoot], root_revision: u64) {
        let previous = std::mem::take(&mut self.by_path);
        self.nodes.clear();
        self.roots.clear();
        for root in roots {
            let id = previous
                .get(&root.identity.canonical_path)
                .copied()
                .unwrap_or_else(|| self.allocate());
            self.by_path
                .insert(root.identity.canonical_path.clone(), id);
            self.roots.push(id);
            self.nodes.insert(
                id,
                ExplorerNode {
                    id,
                    parent: None,
                    root: root.id,
                    path: root.identity.canonical_path.clone(),
                    name: root.name.clone(),
                    kind: ExplorerKind::Root,
                    load: ExplorerLoadState::Unloaded,
                    expanded: false,
                    children: Vec::new(),
                },
            );
        }
        self.selected = self
            .selected
            .filter(|id| self.nodes.contains_key(id))
            .or(self.roots.first().copied());
        self.root_revision = root_revision;
        self.rebuild_visible_projection();
    }

    fn allocate(&mut self) -> ExplorerNodeId {
        self.next_id = self.next_id.saturating_add(1);
        ExplorerNodeId(self.next_id)
    }

    fn remove_subtree(&mut self, id: ExplorerNodeId) {
        if let Some(node) = self.nodes.remove(&id) {
            for child in node.children {
                self.remove_subtree(child);
            }
            self.by_path.remove(&node.path);
        }
    }

    pub fn begin_load(&mut self, node: ExplorerNodeId, generation: u64) -> bool {
        let Some(node) = self.nodes.get_mut(&node) else {
            return false;
        };
        if !matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory) {
            return false;
        }
        node.load = ExplorerLoadState::Loading(generation);
        self.rebuild_visible_projection();
        true
    }

    pub fn apply_load(
        &mut self,
        node: ExplorerNodeId,
        generation: u64,
        root_revision: u64,
        children: Vec<ExplorerChild>,
    ) -> bool {
        if root_revision != self.root_revision
            || !self
                .nodes
                .get(&node)
                .is_some_and(|node| node.load == ExplorerLoadState::Loading(generation))
        {
            return false;
        }
        let root = self.nodes[&node].root;
        let old_children = self.nodes[&node].children.clone();
        let old_ids_by_path = old_children
            .iter()
            .filter_map(|id| self.nodes.get(id).map(|child| (child.path.clone(), *id)))
            .collect::<HashMap<_, _>>();
        let mut reused = BTreeSet::new();
        let mut ids = Vec::new();
        for child in children {
            let existing = old_ids_by_path
                .get(&child.path)
                .copied()
                .filter(|id| self.nodes.get(id).is_some_and(|old| old.kind == child.kind));
            let id = if let Some(id) = existing {
                if let Some(old) = self.nodes.get_mut(&id) {
                    old.parent = Some(node);
                    old.root = root;
                    old.name = child.name;
                    old.path = child.path.clone();
                }
                reused.insert(id);
                id
            } else {
                if let Some(stale) = old_ids_by_path.get(&child.path).copied() {
                    self.remove_subtree(stale);
                }
                let id = self
                    .by_path
                    .get(&child.path)
                    .copied()
                    .unwrap_or_else(|| self.allocate());
                self.by_path.insert(child.path.clone(), id);
                self.nodes.insert(
                    id,
                    ExplorerNode {
                        id,
                        parent: Some(node),
                        root,
                        path: child.path,
                        name: child.name,
                        kind: child.kind,
                        load: ExplorerLoadState::Unloaded,
                        expanded: false,
                        children: Vec::new(),
                    },
                );
                id
            };
            ids.push(id);
        }
        for old in old_children {
            if !reused.contains(&old) && !ids.contains(&old) {
                self.remove_subtree(old);
            }
        }
        if self
            .selected
            .is_some_and(|selected| !self.nodes.contains_key(&selected))
        {
            self.selected = Some(node);
        }
        if let Some(parent) = self.nodes.get_mut(&node) {
            parent.children = ids;
            parent.load = ExplorerLoadState::Loaded;
            parent.expanded = true;
        }
        self.rebuild_visible_projection();
        true
    }

    pub fn set_error(&mut self, node: ExplorerNodeId, generation: u64) {
        let mut changed = false;
        if let Some(node) = self.nodes.get_mut(&node)
            && node.load == ExplorerLoadState::Loading(generation)
        {
            node.load = ExplorerLoadState::Error;
            changed = true;
        }
        if changed {
            self.rebuild_visible_projection();
        }
    }

    pub fn toggle_loaded(&mut self, id: ExplorerNodeId) {
        let mut changed = false;
        if let Some(node) = self.nodes.get_mut(&id)
            && node.load == ExplorerLoadState::Loaded
        {
            node.expanded = !node.expanded;
            changed = true;
        }
        if changed {
            self.rebuild_visible_projection();
        }
    }

    pub fn select_first_child(&mut self) {
        if let Some(id) = self.selected
            && let Some(child) = self
                .nodes
                .get(&id)
                .and_then(|node| node.children.first())
                .copied()
        {
            self.selected = Some(child);
        }
    }

    #[must_use]
    pub fn id_for_path(&self, path: &Path) -> Option<ExplorerNodeId> {
        self.by_path.get(path).copied()
    }

    pub const fn selected_id(&self) -> Option<ExplorerNodeId> {
        self.selected
    }

    /// Return whether each node in the root-to-row lineage is its parent's last child.
    /// Presentation owners turn this structural data into configured tree glyphs.
    #[must_use]
    pub fn tree_lineage(&self, id: ExplorerNodeId) -> Vec<bool> {
        let mut lineage = Vec::new();
        let mut current = id;
        while let Some(parent) = self.nodes.get(&current).and_then(|node| node.parent) {
            let is_last = self
                .nodes
                .get(&parent)
                .and_then(|node| node.children.last())
                .is_some_and(|last| *last == current);
            lineage.push(is_last);
            current = parent;
        }
        lineage.reverse();
        lineage
    }

    #[must_use]
    pub fn selected_node(&self) -> Option<&ExplorerNode> {
        self.selected.and_then(|id| self.nodes.get(&id))
    }
    #[must_use]
    pub fn node(&self, id: ExplorerNodeId) -> Option<&ExplorerNode> {
        self.nodes.get(&id)
    }
    #[must_use]
    pub fn visible(&self) -> Vec<&ExplorerNode> {
        self.visible_nodes
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .collect()
    }
    #[must_use]
    pub fn visible_ids(&self) -> &[ExplorerNodeId] {
        &self.visible_nodes
    }
    #[must_use]
    pub fn visible_index(&self, id: ExplorerNodeId) -> Option<usize> {
        self.visible_positions.get(&id).copied()
    }
    #[must_use]
    pub const fn projection_generation(&self) -> u64 {
        self.projection_generation
    }
    #[must_use]
    pub fn watched_directories(&self) -> Vec<PathBuf> {
        self.nodes
            .values()
            .filter(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && node.expanded
                    && matches!(
                        node.load,
                        ExplorerLoadState::Loaded | ExplorerLoadState::Loading(_)
                    )
            })
            .map(|node| node.path.clone())
            .collect()
    }
    fn rebuild_visible_projection(&mut self) {
        let mut visible = Vec::new();
        for root in &self.roots {
            self.collect_visible_ids(*root, &mut visible);
        }
        self.visible_positions.clear();
        self.visible_positions
            .extend(visible.iter().enumerate().map(|(index, id)| (*id, index)));
        self.visible_nodes = visible;
        self.projection_generation = self.projection_generation.saturating_add(1);
    }
    fn collect_visible_ids(&self, id: ExplorerNodeId, out: &mut Vec<ExplorerNodeId>) {
        if let Some(node) = self.nodes.get(&id) {
            out.push(id);
            if node.expanded {
                for child in &node.children {
                    self.collect_visible_ids(*child, out);
                }
            }
        }
    }
    pub fn move_selection(&mut self, delta: isize) {
        self.move_selection_from(delta, 0);
    }
    pub fn move_selection_from(&mut self, delta: isize, first_presented: usize) {
        if first_presented >= self.visible_nodes.len() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| self.visible_positions.get(&id).copied())
            .unwrap_or(first_presented)
            .max(first_presented);
        let next = (current as isize).saturating_add(delta).clamp(
            first_presented as isize,
            self.visible_nodes.len() as isize - 1,
        ) as usize;
        self.selected = Some(self.visible_nodes[next]);
    }
    pub fn ensure_selection_from(&mut self, first_presented: usize) {
        if first_presented >= self.visible_nodes.len() {
            return;
        }
        let selected_is_presented = self
            .selected
            .and_then(|id| self.visible_positions.get(&id))
            .is_some_and(|index| *index >= first_presented);
        if !selected_is_presented {
            self.selected = Some(self.visible_nodes[first_presented]);
        }
    }
    pub fn select(&mut self, id: ExplorerNodeId) {
        if self.nodes.contains_key(&id) {
            self.selected = Some(id)
        }
    }
    pub fn collapse_or_parent(&mut self) {
        self.collapse_or_parent_from(0);
    }
    pub fn collapse_or_parent_from(&mut self, first_presented: usize) {
        let Some(id) = self.selected else { return };
        if self.nodes.get(&id).is_some_and(|n| n.expanded) {
            if let Some(n) = self.nodes.get_mut(&id) {
                n.expanded = false;
                self.rebuild_visible_projection();
            }
        } else if let Some(parent) = self.nodes.get(&id).and_then(|n| n.parent)
            && self
                .visible_positions
                .get(&parent)
                .is_some_and(|index| *index >= first_presented)
        {
            self.selected = Some(parent)
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticOwner(pub String);

impl DiagnosticOwner {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(WorkspaceError::InvalidDiagnosticOwner);
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedDiagnostic {
    pub uri: String,
    pub range: ByteRange,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub uri: String,
    pub version: Option<BufferRevision>,
    pub range: ByteRange,
    pub severity: DiagnosticSeverity,
    pub source: String,
    pub code: Option<String>,
    pub message: String,
    pub related: Vec<RelatedDiagnostic>,
    pub tags: Vec<String>,
    pub fixes: Vec<String>,
    pub timestamp: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProblemRow {
    pub owner: DiagnosticOwner,
    pub uri: String,
    pub range: ByteRange,
    pub severity: DiagnosticSeverity,
    pub source: String,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct DiagnosticStore {
    maximum_total: usize,
    maximum_per_owner_uri: usize,
    sets: BTreeMap<(DiagnosticOwner, String), Vec<Diagnostic>>,
    total: usize,
}

impl DiagnosticStore {
    #[must_use]
    pub fn new(maximum_total: usize, maximum_per_owner_uri: usize) -> Self {
        Self {
            maximum_total: maximum_total.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT),
            maximum_per_owner_uri: maximum_per_owner_uri.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT),
            sets: BTreeMap::new(),
            total: 0,
        }
    }

    pub fn set_limits(&mut self, maximum_total: usize, maximum_per_owner_uri: usize) {
        self.maximum_total = maximum_total.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT);
        self.maximum_per_owner_uri = maximum_per_owner_uri.clamp(1, ABSOLUTE_DIAGNOSTIC_LIMIT);
        for diagnostics in self.sets.values_mut() {
            diagnostics.truncate(self.maximum_per_owner_uri);
        }
        let mut remaining = self.maximum_total;
        self.sets.retain(|_, diagnostics| {
            diagnostics.truncate(remaining);
            remaining = remaining.saturating_sub(diagnostics.len());
            !diagnostics.is_empty()
        });
        self.total = self.sets.values().map(Vec::len).sum();
    }

    pub fn replace(
        &mut self,
        owner: DiagnosticOwner,
        uri: String,
        version: Option<BufferRevision>,
        current_revision: Option<BufferRevision>,
        mut diagnostics: Vec<Diagnostic>,
    ) -> Result<(), WorkspaceError> {
        if version.is_some() && current_revision != version {
            return Err(WorkspaceError::StaleDiagnostics {
                expected: current_revision,
                actual: version,
            });
        }
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.uri != uri || diagnostic.version != version)
        {
            return Err(WorkspaceError::MixedDiagnosticSet);
        }
        diagnostics.truncate(self.maximum_per_owner_uri);
        let key = (owner, uri);
        let previous = self.sets.get(&key).map_or(0, Vec::len);
        let available = self
            .maximum_total
            .saturating_sub(self.total.saturating_sub(previous));
        diagnostics.truncate(available);
        if diagnostics.is_empty() {
            self.sets.remove(&key);
        } else {
            self.sets.insert(key, diagnostics);
        }
        self.total = self.sets.values().map(Vec::len).sum();
        Ok(())
    }

    pub fn clear_owner_uri(&mut self, owner: &DiagnosticOwner, uri: &str) {
        self.sets.remove(&(owner.clone(), uri.to_owned()));
        self.total = self.sets.values().map(Vec::len).sum();
    }

    pub fn clear_owner(&mut self, owner: &DiagnosticOwner) {
        self.sets.retain(|(candidate, _), _| candidate != owner);
        self.total = self.sets.values().map(Vec::len).sum();
    }

    pub fn clear_uri(&mut self, uri: &str) {
        self.sets.retain(|(_, candidate), _| candidate != uri);
        self.total = self.sets.values().map(Vec::len).sum();
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.total
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total == 0
    }

    #[must_use]
    pub fn problems(
        &self,
        severity: Option<DiagnosticSeverity>,
        source: Option<&str>,
    ) -> Vec<ProblemRow> {
        let mut rows = self
            .sets
            .iter()
            .flat_map(|((owner, _), diagnostics)| {
                diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        severity.is_none_or(|value| value == diagnostic.severity)
                            && source.is_none_or(|value| value == diagnostic.source)
                    })
                    .map(|diagnostic| ProblemRow {
                        owner: owner.clone(),
                        uri: diagnostic.uri.clone(),
                        range: diagnostic.range,
                        severity: diagnostic.severity,
                        source: diagnostic.source.clone(),
                        code: diagnostic.code.clone(),
                        message: diagnostic.message.clone(),
                    })
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.uri
                .cmp(&right.uri)
                .then(left.range.start.cmp(&right.range.start))
                .then(left.severity.cmp(&right.severity))
        });
        rows
    }

    #[must_use]
    pub fn adjacent(&self, uri: &str, offset: ByteOffset, direction: i8) -> Option<ProblemRow> {
        let rows = self
            .problems(None, None)
            .into_iter()
            .filter(|row| row.uri == uri)
            .collect::<Vec<_>>();
        if direction < 0 {
            rows.iter()
                .rev()
                .find(|row| row.range.start < offset)
                .or_else(|| rows.last())
                .cloned()
        } else {
            rows.iter()
                .find(|row| row.range.start > offset)
                .or_else(|| rows.first())
                .cloned()
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PickerItemId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerAction {
    OpenLocation { uri: String, range: ByteRange },
    InvokeCommand { command: String },
    ApplyCodeAction { index: usize },
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerItem {
    pub id: PickerItemId,
    pub label: String,
    pub detail: String,
    pub source: String,
    pub action: PickerAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerPresentation {
    pub title: String,
    pub query: String,
    pub rows: Vec<(PickerItemId, String, String, String)>,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
}

#[derive(Clone, Debug)]
pub struct PickerState {
    generation: u64,
    title: String,
    query: String,
    items: Vec<PickerItem>,
    visible: Vec<usize>,
    selected: Option<usize>,
    scroll: usize,
    maximum_items: usize,
    loading: bool,
}

impl PickerState {
    #[must_use]
    pub fn new(generation: u64, title: String, maximum_items: usize) -> Self {
        Self {
            generation,
            title,
            query: String::new(),
            items: Vec::new(),
            visible: Vec::new(),
            selected: None,
            scroll: 0,
            maximum_items: maximum_items.clamp(1, ABSOLUTE_PICKER_ITEM_LIMIT),
            loading: true,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn append(
        &mut self,
        generation: u64,
        items: impl IntoIterator<Item = PickerItem>,
        complete: bool,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let remaining = self.maximum_items.saturating_sub(self.items.len());
        let mut known = self
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<HashSet<_>>();
        self.items.extend(
            items
                .into_iter()
                .filter(|item| known.insert(item.id.clone()))
                .take(remaining),
        );
        self.loading = !complete;
        self.recompute();
        true
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.recompute();
    }

    pub fn insert_query(&mut self, text: &str) {
        self.query.push_str(text);
        self.recompute();
    }

    pub fn backspace_query(&mut self) {
        self.query.pop();
        self.recompute();
    }

    pub fn move_selection(&mut self, direction: i8, wrap: bool) {
        if self.visible.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected.unwrap_or(0).min(self.visible.len() - 1);
        self.selected = Some(if direction < 0 {
            if current == 0 && wrap {
                self.visible.len() - 1
            } else {
                current.saturating_sub(1)
            }
        } else if current + 1 == self.visible.len() && wrap {
            0
        } else {
            (current + 1).min(self.visible.len() - 1)
        });
    }

    pub fn ensure_selected_visible(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        let Some(selected) = self.selected else {
            self.scroll = 0;
            return;
        };
        if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll.saturating_add(visible_rows) {
            self.scroll = selected.saturating_add(1).saturating_sub(visible_rows);
        }
    }

    #[must_use]
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.selected
            .and_then(|selected| self.visible.get(selected))
            .and_then(|index| self.items.get(*index))
    }

    #[must_use]
    pub fn presentation(&self) -> PickerPresentation {
        PickerPresentation {
            title: self.title.clone(),
            query: self.query.clone(),
            rows: self
                .visible
                .iter()
                .filter_map(|index| self.items.get(*index))
                .map(|item| {
                    (
                        item.id.clone(),
                        item.label.clone(),
                        item.detail.clone(),
                        item.source.clone(),
                    )
                })
                .collect(),
            selected: self.selected,
            scroll: self.scroll,
            loading: self.loading,
        }
    }

    fn recompute(&mut self) {
        let mut scored = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                fuzzy_score(&self.query, &item.label, &item.detail).map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(left_index, left_score), (right_index, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    self.items[*left_index]
                        .label
                        .cmp(&self.items[*right_index].label)
                })
                .then(left_index.cmp(right_index))
        });
        let selected_id = self.selected_item().map(|item| item.id.clone());
        self.visible = scored.into_iter().map(|(index, _)| index).collect();
        self.selected = selected_id
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.items[*index].id == id)
            })
            .or_else(|| (!self.visible.is_empty()).then_some(0));
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
    }
}

fn fuzzy_score(query: &str, label: &str, detail: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let haystack = format!("{label} {detail}").to_lowercase();
    let mut previous = None;
    let mut score = 0_i64;
    let mut search_start = 0;
    for needle in query.to_lowercase().chars() {
        let relative = haystack.get(search_start..)?.find(needle)?;
        let position = search_start + relative;
        score += 100 - i64::try_from(position).unwrap_or(100);
        if previous.is_some_and(|previous| previous + needle.len_utf8() == position) {
            score += 25;
        }
        previous = Some(position);
        search_start = position + needle.len_utf8();
    }
    Some(score)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProposalId(pub u64);

#[derive(Clone, Debug)]
pub struct WorkspaceEditDraft {
    pub canonical_path: PathBuf,
    pub expected_revision: Option<BufferRevision>,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub canonical_path: PathBuf,
    pub revision: Option<BufferRevision>,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposedFileEdit {
    pub canonical_path: PathBuf,
    pub base_revision: Option<BufferRevision>,
    pub original_bytes: Vec<u8>,
    pub proposed_bytes: Vec<u8>,
    pub selected: bool,
    pub edit_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalState {
    Preview,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug)]
pub struct WorkspaceEditProposal {
    pub id: ProposalId,
    pub label: String,
    pub files: Vec<ProposedFileEdit>,
    pub state: ProposalState,
}

impl WorkspaceEditProposal {
    pub fn build(
        id: ProposalId,
        label: String,
        workspace: &WorkspaceState,
        drafts: Vec<WorkspaceEditDraft>,
        snapshots: Vec<FileSnapshot>,
    ) -> Result<Self, WorkspaceError> {
        if drafts.len() > ABSOLUTE_PROPOSAL_FILE_LIMIT {
            return Err(WorkspaceError::ProposalFileLimit);
        }
        let edit_count = drafts.iter().map(|draft| draft.edits.len()).sum::<usize>();
        if edit_count > ABSOLUTE_PROPOSAL_EDIT_LIMIT {
            return Err(WorkspaceError::ProposalEditLimit);
        }
        let snapshots = snapshots
            .into_iter()
            .map(|snapshot| (snapshot.canonical_path.clone(), snapshot))
            .collect::<HashMap<_, _>>();
        let mut files = Vec::with_capacity(drafts.len());
        let mut seen = HashSet::new();
        for draft in drafts {
            workspace.require_path(&draft.canonical_path)?;
            if !seen.insert(draft.canonical_path.clone()) {
                return Err(WorkspaceError::DuplicateProposalTarget(
                    draft.canonical_path,
                ));
            }
            let snapshot = snapshots
                .get(&draft.canonical_path)
                .ok_or_else(|| WorkspaceError::MissingSnapshot(draft.canonical_path.clone()))?;
            if draft.expected_revision != snapshot.revision {
                return Err(WorkspaceError::StaleProposal {
                    path: draft.canonical_path,
                    expected: draft.expected_revision,
                    actual: snapshot.revision,
                });
            }
            let mut text = TextBuffer::from_bytes(&snapshot.bytes)
                .map_err(|error| WorkspaceError::InvalidText(error.to_string()))?;
            let base_revision = text.revision();
            let edit_set = TextEditSet::new(base_revision, draft.edits.clone());
            text.validate_edits(&edit_set)
                .map_err(|error| WorkspaceError::InvalidEdit(error.to_string()))?;
            text.apply_edits(&edit_set)
                .map_err(|error| WorkspaceError::InvalidEdit(error.to_string()))?;
            files.push(ProposedFileEdit {
                canonical_path: snapshot.canonical_path.clone(),
                base_revision: snapshot.revision,
                original_bytes: snapshot.bytes.clone(),
                proposed_bytes: text.to_bytes(),
                selected: true,
                edit_count: draft.edits.len(),
            });
        }
        Ok(Self {
            id,
            label,
            files,
            state: ProposalState::Preview,
        })
    }

    pub fn toggle_file(&mut self, index: usize) -> Result<(), WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        let file = self
            .files
            .get_mut(index)
            .ok_or(WorkspaceError::UnknownProposalFile(index))?;
        file.selected = !file.selected;
        Ok(())
    }

    pub fn accept(&mut self) -> Result<Vec<ProposedFileEdit>, WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        self.state = ProposalState::Accepted;
        Ok(self
            .files
            .iter()
            .filter(|file| file.selected)
            .cloned()
            .collect())
    }

    pub fn reject(&mut self) -> Result<(), WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        self.state = ProposalState::Rejected;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace has no roots")]
    NoRoots,
    #[error("workspace has more than {0} roots")]
    TooManyRoots(usize),
    #[error("workspace root name is empty")]
    EmptyRootName,
    #[error("workspace root is duplicated: {0}")]
    DuplicateRoot(PathBuf),
    #[error("workspace root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("unknown workspace root {0:?}")]
    UnknownRoot(RootId),
    #[error("workspace must retain at least one root")]
    LastRoot,
    #[error("path is outside every workspace root: {0}")]
    PathOutsideRoots(PathBuf),
    #[error("workspace trust is required for {0:?}")]
    TrustRequired(ExecutionSource),
    #[error("invalid diagnostic owner")]
    InvalidDiagnosticOwner,
    #[error("diagnostics are stale: current {expected:?}, result {actual:?}")]
    StaleDiagnostics {
        expected: Option<BufferRevision>,
        actual: Option<BufferRevision>,
    },
    #[error("diagnostic replacement mixes URI or version ownership")]
    MixedDiagnosticSet,
    #[error("workspace edit exceeds the file safety limit")]
    ProposalFileLimit,
    #[error("workspace edit exceeds the edit safety limit")]
    ProposalEditLimit,
    #[error("workspace edit contains duplicate target {0}")]
    DuplicateProposalTarget(PathBuf),
    #[error("workspace edit has no captured snapshot for {0}")]
    MissingSnapshot(PathBuf),
    #[error("workspace edit for {path} is stale: expected {expected:?}, found {actual:?}")]
    StaleProposal {
        path: PathBuf,
        expected: Option<BufferRevision>,
        actual: Option<BufferRevision>,
    },
    #[error("workspace edit text is invalid: {0}")]
    InvalidText(String),
    #[error("workspace edit is invalid: {0}")]
    InvalidEdit(String),
    #[error("workspace edit proposal is already closed")]
    ProposalClosed,
    #[error("workspace edit has no file at index {0}")]
    UnknownProposalFile(usize),
    #[error("cannot {operation} `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn workspace(root: &Path) -> WorkspaceState {
        WorkspaceState::new(
            WorkspaceId(1),
            [(
                "root".into(),
                RootIdentity::capture(root).expect("identity"),
            )],
        )
        .expect("workspace")
    }

    #[test]
    fn multi_root_prefers_the_narrowest_containing_root_and_defaults_untrusted() {
        let directory = tempdir().expect("tempdir");
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).expect("nested");
        let workspace = WorkspaceState::new(
            WorkspaceId(7),
            [
                (
                    "outer".into(),
                    RootIdentity::capture(directory.path()).expect("outer"),
                ),
                (
                    "inner".into(),
                    RootIdentity::capture(&nested).expect("inner"),
                ),
            ],
        )
        .expect("workspace");
        assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
        assert_eq!(
            workspace
                .root_for_canonical_path(&nested.join("file"))
                .map(|root| root.name.as_str()),
            Some("inner")
        );
        assert!(
            workspace
                .require_path(Path::new("/definitely/outside"))
                .is_err()
        );
        assert!(
            workspace
                .require_path(&directory.path().join("nested/../../escape"))
                .is_err()
        );
    }

    #[test]
    fn root_ids_are_monotonic_removal_has_fallback_and_root_changes_revoke_trust() {
        let parent = tempdir().expect("parent");
        let first = parent.path().join("first");
        let second = parent.path().join("second");
        let third = parent.path().join("third");
        fs::create_dir(&first).expect("first");
        fs::create_dir(&second).expect("second");
        fs::create_dir(&third).expect("third");
        let mut workspace = workspace(&first);
        workspace.set_trust(WorkspaceTrust::Trusted);
        let second_id = workspace
            .add_root(
                "second".into(),
                RootIdentity::capture(&second).expect("identity"),
            )
            .expect("add second");
        assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
        workspace.set_trust(WorkspaceTrust::Trusted);
        let removed = workspace.remove_root(second_id).expect("remove active");
        assert_eq!(removed.id, second_id);
        assert_eq!(
            workspace.active_root(),
            workspace.roots().first().map(|root| root.id)
        );
        assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
        let third_id = workspace
            .add_root(
                "third".into(),
                RootIdentity::capture(&third).expect("identity"),
            )
            .expect("add third");
        assert!(third_id.0 > second_id.0, "root IDs must not be recycled");
        assert!(matches!(
            workspace.add_root(
                "duplicate".into(),
                RootIdentity {
                    canonical_path: fs::canonicalize(&third).expect("canonical"),
                    filesystem_identity: Some((u64::MAX, u64::MAX)),
                }
            ),
            Err(WorkspaceError::DuplicateRoot(_))
        ));
        workspace.remove_root(third_id).expect("remove third");
        assert!(matches!(
            workspace.remove_root(workspace.roots()[0].id),
            Err(WorkspaceError::LastRoot)
        ));
    }

    #[test]
    fn workspace_enforces_absolute_root_ceiling() {
        let parent = tempdir().expect("parent");
        let first = parent.path().join("root-0");
        fs::create_dir(&first).expect("first");
        let mut workspace = workspace(&first);
        for index in 1..ABSOLUTE_ROOT_LIMIT {
            let path = parent.path().join(format!("root-{index}"));
            fs::create_dir(&path).expect("root");
            workspace
                .add_root(
                    format!("root-{index}"),
                    RootIdentity::capture(&path).expect("identity"),
                )
                .expect("within limit");
        }
        let overflow = parent.path().join("overflow");
        fs::create_dir(&overflow).expect("overflow");
        assert!(matches!(
            workspace.add_root(
                "overflow".into(),
                RootIdentity::capture(&overflow).expect("identity")
            ),
            Err(WorkspaceError::TooManyRoots(ABSOLUTE_ROOT_LIMIT))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn trust_identity_does_not_follow_a_retargeted_symlink() {
        use std::os::unix::fs::symlink;
        let directory = tempdir().expect("tempdir");
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::create_dir(&first).expect("first");
        fs::create_dir(&second).expect("second");
        let link = directory.path().join("root");
        symlink(&first, &link).expect("link");
        let original = RootIdentity::capture(&link).expect("original");
        let mut store = TrustStore::default();
        store.trust(original.clone());
        fs::remove_file(&link).expect("unlink");
        symlink(&second, &link).expect("retarget");
        let changed = RootIdentity::capture(&link).expect("changed");
        assert!(store.is_trusted(&original));
        assert!(!store.is_trusted(&changed));
    }

    #[test]
    fn trust_blocks_project_execution_but_not_system_tools() {
        let directory = tempdir().expect("tempdir");
        let mut workspace = workspace(directory.path());
        assert!(
            workspace
                .authorize_execution(ExecutionSource::SystemTool)
                .is_ok()
        );
        assert!(matches!(
            workspace.authorize_execution(ExecutionSource::ProjectLocal),
            Err(WorkspaceError::TrustRequired(_))
        ));
        workspace.set_trust(WorkspaceTrust::Trusted);
        assert!(
            workspace
                .authorize_execution(ExecutionSource::ProjectLocal)
                .is_ok()
        );
    }

    fn diagnostic(uri: &str, version: u64, start: usize, message: &str) -> Diagnostic {
        Diagnostic {
            uri: uri.into(),
            version: Some(BufferRevision(version)),
            range: ByteRange::new(ByteOffset(start), ByteOffset(start + 1)).expect("range"),
            severity: DiagnosticSeverity::Warning,
            source: "fake".into(),
            code: None,
            message: message.into(),
            related: Vec::new(),
            tags: Vec::new(),
            fixes: Vec::new(),
            timestamp: crate::clock::system_time(),
        }
    }

    #[test]
    fn trust_database_round_trips_exact_filesystem_identity_and_rejects_path_only_records() {
        let root = tempdir().expect("root");
        let state = tempdir().expect("state");
        let path = state.path().join("trust.toml");
        let identity = RootIdentity::capture(root.path()).expect("identity");
        let database = TrustDatabase {
            version: TRUST_STATE_VERSION,
            identities: vec![identity.clone()],
        };
        persist_trust_database(&path, &database).expect("persist");
        assert_eq!(load_trust_database(&path).expect("load"), database);
        let changed = RootIdentity {
            canonical_path: identity.canonical_path.clone(),
            filesystem_identity: identity
                .filesystem_identity
                .map(|(device, inode)| (device, inode.saturating_add(1))),
        };
        let mut store = TrustStore::default();
        store.replace(load_trust_database(&path).expect("load").identities);
        assert!(!store.is_trusted(&changed));
        assert!(matches!(
            persist_trust_database(
                &path,
                &TrustDatabase {
                    version: TRUST_STATE_VERSION,
                    identities: vec![RootIdentity {
                        canonical_path: identity.canonical_path,
                        filesystem_identity: None,
                    }],
                }
            ),
            Err(TrustStateError::MissingFilesystemIdentity(_))
        ));
    }

    #[test]
    fn trust_database_missing_corrupt_oversize_and_atomic_failure_are_safe() {
        let state = tempdir().expect("state");
        let missing = state.path().join("missing");
        assert!(
            load_trust_database(&missing)
                .expect("missing")
                .identities
                .is_empty()
        );
        fs::write(&missing, "not = [valid").expect("corrupt");
        assert!(matches!(
            load_trust_database(&missing),
            Err(TrustStateError::Invalid { .. })
        ));
        fs::write(&missing, vec![b'x'; TRUST_STATE_BYTE_LIMIT as usize + 1]).expect("large");
        assert!(matches!(
            load_trust_database(&missing),
            Err(TrustStateError::TooLarge(_))
        ));
        let parent_file = state.path().join("parent-file");
        fs::write(&parent_file, "occupied").expect("parent file");
        assert!(
            persist_trust_database(&parent_file.join("trust.toml"), &TrustDatabase::default())
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_trust_does_not_follow_replaced_root_inode() {
        let parent = tempdir().expect("parent");
        let root = parent.path().join("root");
        fs::create_dir(&root).expect("root");
        let original = RootIdentity::capture(&root).expect("original");
        let displaced = parent.path().join("displaced");
        fs::rename(&root, &displaced).expect("displace");
        fs::create_dir(&root).expect("replacement");
        let replacement = RootIdentity::capture(&root).expect("replacement identity");
        assert_eq!(original.canonical_path, replacement.canonical_path);
        assert_ne!(
            original.filesystem_identity,
            replacement.filesystem_identity
        );
        let mut store = TrustStore::default();
        store.trust(original);
        assert!(!store.is_trusted(&replacement));
    }

    #[test]
    fn trust_worker_reports_distinct_generations_and_async_identity() {
        let root = tempdir().expect("root");
        let state = tempdir().expect("state");
        let mut service = WorkspaceTrustService::start(4);
        let identity_generation = service
            .inspect_root(root.path().to_path_buf())
            .expect("inspect");
        let load_generation = service.load(state.path().join("missing")).expect("load");
        assert!(load_generation > identity_generation);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut identity_seen = false;
        let mut load_seen = false;
        while !identity_seen || !load_seen {
            if let Some(event) = service.try_event().expect("event") {
                match event {
                    TrustEvent::RootInspected { generation, result } => {
                        assert_eq!(generation, identity_generation);
                        assert!(result.expect("identity").filesystem_identity.is_some());
                        identity_seen = true;
                    }
                    TrustEvent::Loaded { generation, result } => {
                        assert_eq!(generation, load_generation);
                        assert!(result.expect("database").identities.is_empty());
                        load_seen = true;
                    }
                    TrustEvent::Persisted { .. } => panic!("unexpected persist"),
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker timeout");
            std::thread::yield_now();
        }
    }

    #[test]
    fn explorer_state_is_lazy_stable_and_rejects_stale_loads() {
        let root = tempdir().expect("root");
        let workspace = workspace(root.path());
        let mut explorer = ExplorerState::default();
        explorer.reset_roots(workspace.roots(), 3);
        let root_generation = explorer.projection_generation();
        let root_id = explorer.selected_node().expect("selected root").id;
        assert_eq!(explorer.visible_ids(), &[root_id]);
        assert!(explorer.begin_load(root_id, 9));
        let loading_generation = explorer.projection_generation();
        assert!(loading_generation > root_generation);
        assert!(!explorer.apply_load(root_id, 8, 3, Vec::new()));
        assert!(!explorer.apply_load(root_id, 9, 2, Vec::new()));
        assert_eq!(explorer.projection_generation(), loading_generation);
        let child_path = root.path().join("child");
        assert!(explorer.apply_load(
            root_id,
            9,
            3,
            vec![ExplorerChild {
                path: child_path.clone(),
                name: "child".into(),
                kind: ExplorerKind::Directory
            }]
        ));
        assert_eq!(explorer.visible().len(), 2);
        assert!(explorer.projection_generation() > loading_generation);
        let child_id = explorer.visible_ids()[1];
        assert!(explorer.begin_load(root_id, 10));
        assert!(explorer.apply_load(
            root_id,
            10,
            3,
            vec![ExplorerChild {
                path: child_path,
                name: "child".into(),
                kind: ExplorerKind::Directory,
            }]
        ));
        assert_eq!(explorer.visible_ids()[1], child_id, "stable path ID");
        let loaded_generation = explorer.projection_generation();
        assert!(explorer.begin_load(child_id, 11));
        assert!(explorer.apply_load(
            child_id,
            11,
            3,
            vec![ExplorerChild {
                path: root.path().join("child/nested"),
                name: "nested".into(),
                kind: ExplorerKind::File,
            }]
        ));
        let nested_id = explorer.visible_ids()[2];
        explorer.select(child_id);
        let nested_generation = explorer.projection_generation();
        explorer.move_selection_from(-1, 1);
        assert_eq!(explorer.selected_id(), Some(child_id));
        assert_eq!(explorer.projection_generation(), nested_generation);

        explorer.select(nested_id);
        explorer.collapse_or_parent_from(1);
        assert_eq!(explorer.selected_id(), Some(child_id));
        explorer.collapse_or_parent_from(1);
        let collapsed_generation = explorer.projection_generation();
        assert_eq!(explorer.selected_id(), Some(child_id));
        assert_eq!(explorer.visible_ids(), &[root_id, child_id]);
        explorer.collapse_or_parent_from(1);
        assert_eq!(explorer.selected_id(), Some(child_id));
        assert_eq!(explorer.projection_generation(), collapsed_generation);

        explorer.select(root_id);
        explorer.toggle_loaded(root_id);
        assert!(explorer.projection_generation() > loaded_generation);
        assert_eq!(explorer.visible_ids(), &[root_id]);
        explorer.toggle_loaded(root_id);
        explorer.reset_roots(workspace.roots(), 4);
        assert_eq!(explorer.selected_node().map(|node| node.id), Some(root_id));
    }

    #[test]
    fn explorer_presented_range_hides_single_root_but_keeps_multi_roots_selectable() {
        let first = tempdir().expect("first");
        let second = tempdir().expect("second");
        let workspace = WorkspaceState::new(
            WorkspaceId(9),
            [
                (
                    "first".into(),
                    RootIdentity::capture(first.path()).expect("first identity"),
                ),
                (
                    "second".into(),
                    RootIdentity::capture(second.path()).expect("second identity"),
                ),
            ],
        )
        .expect("workspace");
        let mut explorer = ExplorerState::default();
        explorer.reset_roots(workspace.roots(), workspace.root_revision());
        let roots = explorer.visible_ids().to_vec();
        assert_eq!(roots.len(), 2);
        explorer.move_selection_from(1, 0);
        assert_eq!(explorer.selected_id(), Some(roots[1]));
        explorer.move_selection_from(-1, 0);
        assert_eq!(explorer.selected_id(), Some(roots[0]));
    }

    #[test]
    fn diagnostics_replace_by_owner_reject_stale_and_obey_bounds() {
        let owner = DiagnosticOwner::new("lsp.fake").expect("owner");
        let mut store = DiagnosticStore::new(3, 2);
        store
            .replace(
                owner.clone(),
                "file:///a".into(),
                Some(BufferRevision(1)),
                Some(BufferRevision(1)),
                vec![
                    diagnostic("file:///a", 1, 0, "one"),
                    diagnostic("file:///a", 1, 2, "two"),
                    diagnostic("file:///a", 1, 4, "truncated"),
                ],
            )
            .expect("replace");
        assert_eq!(store.len(), 2);
        assert!(
            store
                .replace(
                    owner.clone(),
                    "file:///a".into(),
                    Some(BufferRevision(1)),
                    Some(BufferRevision(2)),
                    Vec::new(),
                )
                .is_err()
        );
        assert_eq!(
            store
                .adjacent("file:///a", ByteOffset(0), 1)
                .map(|row| row.message),
            Some("two".into())
        );
        store.clear_owner(&owner);
        assert!(store.is_empty());
    }

    #[test]
    fn picker_rejects_stale_streams_deduplicates_and_filters_stably() {
        let mut picker = PickerState::new(4, "References".into(), 3);
        assert!(!picker.append(3, Vec::new(), true));
        let item = |id: &str, label: &str| PickerItem {
            id: PickerItemId(id.into()),
            label: label.into(),
            detail: String::new(),
            source: "fake".into(),
            action: PickerAction::None,
        };
        assert!(picker.append(
            4,
            [
                item("a", "alpha"),
                item("b", "beta"),
                item("a", "duplicate")
            ],
            true
        ));
        picker.set_query("bt".into());
        assert_eq!(
            picker.selected_item().map(|item| item.id.0.as_str()),
            Some("b")
        );
        picker.insert_query("z");
        assert!(picker.selected_item().is_none());
        picker.backspace_query();
        assert_eq!(picker.presentation().rows.len(), 1);
    }

    #[test]
    fn workspace_edit_proposal_validates_scope_overlap_staleness_and_rejection() {
        let directory = tempdir().expect("tempdir");
        let workspace = workspace(directory.path());
        let path = directory.path().join("a.txt");
        fs::write(&path, "abc").expect("write");
        let path = fs::canonicalize(path).expect("canonical");
        let snapshot = FileSnapshot {
            canonical_path: path.clone(),
            revision: Some(BufferRevision(9)),
            bytes: b"abc".to_vec(),
        };
        let draft = WorkspaceEditDraft {
            canonical_path: path.clone(),
            expected_revision: Some(BufferRevision(9)),
            edits: vec![TextEdit::new(
                ByteRange::new(ByteOffset(1), ByteOffset(2)).expect("range"),
                "Z",
            )],
        };
        let mut proposal = WorkspaceEditProposal::build(
            ProposalId(1),
            "rename".into(),
            &workspace,
            vec![draft.clone()],
            vec![snapshot.clone()],
        )
        .expect("proposal");
        assert_eq!(proposal.files[0].proposed_bytes, b"aZc");
        proposal.reject().expect("reject");
        assert_eq!(proposal.state, ProposalState::Rejected);
        assert!(proposal.accept().is_err());

        let stale = WorkspaceEditDraft {
            expected_revision: Some(BufferRevision(8)),
            ..draft.clone()
        };
        assert!(
            WorkspaceEditProposal::build(
                ProposalId(2),
                "stale".into(),
                &workspace,
                vec![stale],
                vec![snapshot.clone()]
            )
            .is_err()
        );
        let overlap = WorkspaceEditDraft {
            edits: vec![
                TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(2)).expect("range"),
                    "x",
                ),
                TextEdit::new(
                    ByteRange::new(ByteOffset(1), ByteOffset(3)).expect("range"),
                    "y",
                ),
            ],
            ..draft
        };
        assert!(
            WorkspaceEditProposal::build(
                ProposalId(3),
                "overlap".into(),
                &workspace,
                vec![overlap],
                vec![snapshot]
            )
            .is_err()
        );
    }
}
