//! Explorer nodes, directory children and visible tree rows.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::workspace::RootId;

use crate::workspace::WorkspaceRoot;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct ExplorerNodeId(pub(in crate::app) u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum ExplorerKind {
    Root,
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum ExplorerLoadState {
    Unloaded,
    Loading(u64),
    Loaded,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct ExplorerChild {
    pub(in crate::app) path: PathBuf,
    pub(in crate::app) name: String,
    pub(in crate::app) kind: ExplorerKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct ExplorerNode {
    pub(in crate::app) id: ExplorerNodeId,
    pub(in crate::app) parent: Option<ExplorerNodeId>,
    pub(in crate::app) root: RootId,
    pub(in crate::app) path: PathBuf,
    pub(in crate::app) name: String,
    pub(in crate::app) kind: ExplorerKind,
    pub(in crate::app) load: ExplorerLoadState,
    pub(in crate::app) children: Vec<ExplorerNodeId>,
}

#[derive(Clone, Debug, Default)]
struct ExplorerData {
    nodes: BTreeMap<ExplorerNodeId, ExplorerNode>,
    by_path: HashMap<PathBuf, ExplorerNodeId>,
    roots: Vec<ExplorerNodeId>,
    next_id: u64,
    root_revision: u64,
}

/// One tree view over shared directory data. Selection, expansion, filter and
/// visible-row projection belong to this view; cloning shares only `data`.
#[derive(Clone, Debug)]
pub(in crate::app) struct ExplorerState {
    data: Arc<ExplorerData>,
    selected: Option<ExplorerNodeId>,
    expanded: BTreeSet<ExplorerNodeId>,
    filter: String,
    visible_nodes: Vec<ExplorerNodeId>,
    visible_positions: HashMap<ExplorerNodeId, usize>,
    projection_revision: u64,
}

impl Default for ExplorerState {
    fn default() -> Self {
        Self {
            data: Arc::new(ExplorerData::default()),
            selected: None,
            expanded: BTreeSet::new(),
            filter: String::new(),
            visible_nodes: Vec::new(),
            visible_positions: HashMap::new(),
            projection_revision: 0,
        }
    }
}

impl ExplorerState {
    /// Clone only view state while retaining the exact shared tree data source.
    #[must_use]
    pub(in crate::app) fn clone_view(&self) -> Self {
        self.clone()
    }

    /// Adopt a newer shared data snapshot without adopting another view's
    /// selection, expansion or filter state.
    pub(in crate::app) fn synchronize_data_from(&mut self, source: &Self) {
        self.data = source.data.clone();
        self.selected = self
            .selected
            .filter(|id| self.data.nodes.contains_key(id))
            .or_else(|| self.data.roots.first().copied());
        self.expanded.retain(|id| self.data.nodes.contains_key(id));
        self.rebuild_visible_projection();
    }

    #[cfg(test)]
    #[must_use]
    pub(in crate::app) fn shares_data_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    #[must_use]
    pub(in crate::app) fn root_revision(&self) -> u64 {
        self.data.root_revision
    }

    pub(in crate::app) fn reset_roots(&mut self, roots: &[WorkspaceRoot], root_revision: u64) {
        let previous = self.data.by_path.clone();
        let data = Arc::make_mut(&mut self.data);
        data.nodes.clear();
        data.by_path.clear();
        data.roots.clear();
        for root in roots {
            let id = previous
                .get(&root.identity.canonical_path)
                .copied()
                .unwrap_or_else(|| allocate(data));
            data.by_path
                .insert(root.identity.canonical_path.clone(), id);
            data.roots.push(id);
            data.nodes.insert(
                id,
                ExplorerNode {
                    id,
                    parent: None,
                    root: root.id,
                    path: root.identity.canonical_path.clone(),
                    name: root.name.clone(),
                    kind: ExplorerKind::Root,
                    load: ExplorerLoadState::Unloaded,
                    children: Vec::new(),
                },
            );
        }
        data.root_revision = root_revision;
        self.selected = self
            .selected
            .filter(|id| data.nodes.contains_key(id))
            .or(data.roots.first().copied());
        self.expanded.retain(|id| data.nodes.contains_key(id));
        self.rebuild_visible_projection();
    }

    pub(in crate::app) fn begin_load(&mut self, node: ExplorerNodeId, request_id: u64) -> bool {
        let data = Arc::make_mut(&mut self.data);
        let Some(node) = data.nodes.get_mut(&node) else {
            return false;
        };
        if !matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory) {
            return false;
        }
        if matches!(
            node.load,
            ExplorerLoadState::Unloaded | ExplorerLoadState::Error
        ) && self.filter.is_empty()
        {
            self.expanded.insert(node.id);
        }
        node.load = ExplorerLoadState::Loading(request_id);
        self.rebuild_visible_projection();
        true
    }

    pub(in crate::app) fn apply_load(
        &mut self,
        node: ExplorerNodeId,
        request_id: u64,
        root_revision: u64,
        children: Vec<ExplorerChild>,
    ) -> bool {
        if root_revision != self.data.root_revision
            || !self
                .data
                .nodes
                .get(&node)
                .is_some_and(|node| node.load == ExplorerLoadState::Loading(request_id))
        {
            return false;
        }
        let data = Arc::make_mut(&mut self.data);
        let root = data.nodes[&node].root;
        let parent_path = data.nodes[&node].path.clone();
        let old_children = data.nodes[&node].children.clone();
        let old_ids_by_path = old_children
            .iter()
            .filter_map(|id| data.nodes.get(id).map(|child| (child.path.clone(), *id)))
            .collect::<HashMap<_, _>>();
        let mut ids = Vec::new();
        for child in children {
            if child.path.parent() != Some(parent_path.as_path())
                || data
                    .by_path
                    .get(&child.path)
                    .is_some_and(|id| data.roots.contains(id))
            {
                continue;
            }
            let existing = old_ids_by_path
                .get(&child.path)
                .copied()
                .filter(|id| data.nodes.get(id).is_some_and(|old| old.kind == child.kind));
            let id = if let Some(id) = existing {
                if let Some(old) = data.nodes.get_mut(&id) {
                    old.parent = Some(node);
                    old.root = root;
                    old.name = child.name;
                    old.path = child.path.clone();
                }
                id
            } else {
                if let Some(stale) = old_ids_by_path.get(&child.path).copied() {
                    remove_subtree(data, stale);
                }
                let id = allocate(data);
                data.by_path.insert(child.path.clone(), id);
                data.nodes.insert(
                    id,
                    ExplorerNode {
                        id,
                        parent: Some(node),
                        root,
                        path: child.path,
                        name: child.name,
                        kind: child.kind,
                        load: ExplorerLoadState::Unloaded,
                        children: Vec::new(),
                    },
                );
                id
            };
            ids.push(id);
        }
        let retained = ids.iter().copied().collect::<HashSet<_>>();
        for old in old_children {
            if !retained.contains(&old) {
                remove_subtree(data, old);
            }
        }
        self.expanded.retain(|id| data.nodes.contains_key(id));
        if self
            .selected
            .is_some_and(|selected| !data.nodes.contains_key(&selected))
        {
            self.selected = Some(node);
        }
        if let Some(parent) = data.nodes.get_mut(&node) {
            parent.children = ids;
            parent.load = ExplorerLoadState::Loaded;
        }
        self.rebuild_visible_projection();
        true
    }

    pub(in crate::app) fn set_error(&mut self, node: ExplorerNodeId, request_id: u64) {
        let mut changed = false;
        if let Some(node) = Arc::make_mut(&mut self.data).nodes.get_mut(&node)
            && node.load == ExplorerLoadState::Loading(request_id)
        {
            node.load = ExplorerLoadState::Error;
            changed = true;
        }
        if changed {
            self.rebuild_visible_projection();
        }
    }

    pub(in crate::app) fn toggle_loaded(&mut self, id: ExplorerNodeId) {
        if !self.data.nodes.get(&id).is_some_and(|node| {
            matches!(
                node.load,
                ExplorerLoadState::Loaded | ExplorerLoadState::Loading(_)
            )
        }) {
            return;
        }
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild_visible_projection();
    }

    pub(in crate::app) fn set_filter(&mut self, filter: impl Into<String>) {
        let filter = filter.into();
        if self.filter != filter {
            self.filter = filter;
            self.rebuild_visible_projection();
        }
    }

    #[must_use]
    pub(in crate::app) fn filter(&self) -> &str {
        &self.filter
    }

    /// A folder not read yet, so a search can read the whole tree, up to `limit` entries.
    #[must_use]
    pub(in crate::app) fn unloaded_directory(&self, limit: usize) -> Option<ExplorerNodeId> {
        if self.data.nodes.len() >= limit {
            return None;
        }
        self.data
            .nodes
            .values()
            .find(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && node.load == ExplorerLoadState::Unloaded
            })
            .map(|node| node.id)
    }

    #[must_use]
    pub(in crate::app) fn is_expanded(&self, id: ExplorerNodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub(in crate::app) fn select_first_child(&mut self) {
        let Some(id) = self.selected else { return };
        if !self.is_expanded(id) {
            self.toggle_loaded(id);
        }
        if let Some(child) = self.data.nodes.get(&id).and_then(|node| {
            node.children
                .iter()
                .find(|child| self.visible_positions.contains_key(child))
                .copied()
        }) {
            self.selected = Some(child);
        }
    }

    #[must_use]
    pub(in crate::app) fn id_for_path(&self, path: &Path) -> Option<ExplorerNodeId> {
        self.data.by_path.get(path).copied()
    }

    pub(in crate::app) const fn selected_id(&self) -> Option<ExplorerNodeId> {
        self.selected
    }

    /// Return whether each node in the root-to-row lineage is its parent's last child.
    #[must_use]
    pub(in crate::app) fn tree_lineage(&self, id: ExplorerNodeId) -> Vec<bool> {
        let mut lineage = Vec::new();
        let mut current = id;
        while let Some(parent) = self.data.nodes.get(&current).and_then(|node| node.parent) {
            let is_last = self
                .data
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
    pub(in crate::app) fn selected_node(&self) -> Option<&ExplorerNode> {
        self.selected.and_then(|id| self.data.nodes.get(&id))
    }
    #[must_use]
    pub(in crate::app) fn node(&self, id: ExplorerNodeId) -> Option<&ExplorerNode> {
        self.data.nodes.get(&id)
    }
    #[must_use]
    #[cfg(test)]
    pub(in crate::app) fn visible(&self) -> Vec<&ExplorerNode> {
        self.visible_nodes
            .iter()
            .filter_map(|id| self.data.nodes.get(id))
            .collect()
    }
    #[must_use]
    pub(in crate::app) fn visible_ids(&self) -> &[ExplorerNodeId] {
        &self.visible_nodes
    }
    #[must_use]
    pub(in crate::app) fn visible_index(&self, id: ExplorerNodeId) -> Option<usize> {
        self.visible_positions.get(&id).copied()
    }
    #[must_use]
    pub(in crate::app) const fn projection_revision(&self) -> u64 {
        self.projection_revision
    }
    #[must_use]
    pub(in crate::app) fn watched_directories(&self) -> Vec<PathBuf> {
        self.data
            .nodes
            .values()
            .filter(|node| self.is_watched_directory(node.id))
            .map(|node| node.path.clone())
            .collect()
    }
    pub(in crate::app) fn is_watched_directory(&self, id: ExplorerNodeId) -> bool {
        self.data.nodes.get(&id).is_some_and(|node| {
            matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                && (self.expanded.contains(&id)
                    || (!self.filter.is_empty() && self.visible_positions.contains_key(&id)))
                && matches!(
                    node.load,
                    ExplorerLoadState::Loaded | ExplorerLoadState::Loading(_)
                )
        })
    }
    fn rebuild_visible_projection(&mut self) {
        let mut visible = Vec::new();
        for root in &self.data.roots {
            self.collect_visible_ids(*root, &mut visible);
        }
        self.visible_positions.clear();
        self.visible_positions
            .extend(visible.iter().enumerate().map(|(index, id)| (*id, index)));
        self.visible_nodes = visible;
        self.projection_revision = self.projection_revision.saturating_add(1);
    }
    fn collect_visible_ids(&self, id: ExplorerNodeId, out: &mut Vec<ExplorerNodeId>) -> bool {
        let Some(node) = self.data.nodes.get(&id) else {
            return false;
        };
        if self.filter.is_empty() {
            out.push(id);
            if self.expanded.contains(&id) {
                for child in &node.children {
                    self.collect_visible_ids(*child, out);
                }
            }
            return true;
        }
        let mut descendants = Vec::new();
        let mut descendant_matches = false;
        for child in &node.children {
            descendant_matches |= self.collect_visible_ids(*child, &mut descendants);
        }
        let matches = node
            .name
            .to_lowercase()
            .contains(&self.filter.to_lowercase());
        if matches || descendant_matches {
            out.push(id);
            out.extend(descendants);
            true
        } else {
            false
        }
    }
    pub(in crate::app) fn move_selection(&mut self, delta: isize) {
        if self.visible_nodes.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| self.visible_positions.get(&id).copied())
            .unwrap_or(0);
        let next = (current as isize)
            .saturating_add(delta)
            .clamp(0, self.visible_nodes.len() as isize - 1) as usize;
        self.selected = Some(self.visible_nodes[next]);
    }
    pub(in crate::app) fn ensure_selection(&mut self) {
        let selected_is_visible = self
            .selected
            .is_some_and(|id| self.visible_positions.contains_key(&id));
        if !selected_is_visible {
            self.selected = self.visible_nodes.first().copied();
        }
    }
    pub(in crate::app) fn select(&mut self, id: ExplorerNodeId) {
        if self.data.nodes.contains_key(&id) {
            self.selected = Some(id)
        }
    }
    pub(in crate::app) fn collapse_or_parent(&mut self) {
        let Some(id) = self.selected else { return };
        if self.expanded.remove(&id) {
            self.rebuild_visible_projection();
        } else if let Some(parent) = self.data.nodes.get(&id).and_then(|node| node.parent)
            && self.visible_positions.contains_key(&parent)
        {
            self.selected = Some(parent)
        }
    }
}

fn allocate(data: &mut ExplorerData) -> ExplorerNodeId {
    data.next_id = data.next_id.saturating_add(1);
    ExplorerNodeId(data.next_id)
}

fn remove_subtree(data: &mut ExplorerData, id: ExplorerNodeId) {
    if let Some(node) = data.nodes.remove(&id) {
        for child in node.children {
            remove_subtree(data, child);
        }
        data.by_path.remove(&node.path);
    }
}
