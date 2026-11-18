//! Workspace roots, active-root selection and trust state.

use std::collections::HashSet;
use std::path::Path;

use crate::workspace::ABSOLUTE_ROOT_LIMIT;

use crate::workspace::edit::WorkspaceError;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RootId(pub u64);

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

impl ExecutionSource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SystemTool => "System tools",
            Self::UserConfigured => "Configured tools",
            Self::ProjectLocal => "Project tools",
            Self::WorkspaceTask => "Tasks",
            Self::ToolInstall => "Tool installs",
            Self::DebugProgram => "Debug sessions",
            Self::PluginCapability => "Plugins",
        }
    }
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

use crate::files::RootIdentity;
