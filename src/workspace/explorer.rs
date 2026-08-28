use super::*;

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
