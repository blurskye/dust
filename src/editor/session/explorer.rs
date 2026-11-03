//! Explorer tree, directory lane, watch and viewport ownership.
mod intent;
use super::workspace::WorkspaceVersion;
use crate::files::{
    DirectoryEntryKind, DirectoryService, ExplorerWatchBatch, ExplorerWatchEvent,
    ExplorerWatchGeneration, ExplorerWatchService,
};
use crate::ui::{ExplorerSegmentRole, ExplorerUiRow, ExplorerUiSegment, ExplorerUiState};
use crate::workspace::{
    ExplorerChild, ExplorerKind, ExplorerLoadState, ExplorerNodeId, ExplorerState, WorkspaceRoot,
};
pub(in crate::editor::session) use intent::{MutationContext, MutationUpdate};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Arc,
};
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::editor::session) struct ExplorerViewId(pub u64);
pub(in crate::editor::session) const PRIMARY_EXPLORER_VIEW: ExplorerViewId = ExplorerViewId(1);

#[derive(Clone, Debug)]
struct ExplorerViewState {
    tree: ExplorerState,
    scroll: usize,
}
type PendingDirectoryLoad = (
    ExplorerViewId,
    ExplorerNodeId,
    u64,
    u64,
    WorkspaceVersion,
    u64,
    Arc<()>,
);

#[derive(Debug)]
pub(in crate::editor::session) struct ExplorerCloneCandidate {
    identity: Arc<()>,
    source: Arc<()>,
    id: ExplorerViewId,
    view: ExplorerViewState,
}
impl ExplorerCloneCandidate {
    pub(in crate::editor::session) fn id(&self) -> ExplorerViewId {
        self.id
    }
}

/// How many entries a search reads at most, so a huge tree does not stall it.
const SEARCH_ENTRY_LIMIT: usize = 50_000;

pub(in crate::editor::session) struct ExplorerSession {
    primary: ExplorerViewState,
    cloned_views: BTreeMap<ExplorerViewId, ExplorerViewState>,
    active_view: ExplorerViewId,
    next_view: u64,
    view_identity: Arc<()>,
    view_source: Arc<()>,
    mutation: Option<intent::MutationIntent>,
    rename: Option<intent::RenamePrompt>,
    open: Option<intent::OpenIntent>,
    clipboard: Option<(PathBuf, bool, WorkspaceVersion)>,
    directory: DirectoryService,
    pending: Option<PendingDirectoryLoad>,
    epoch: Arc<()>,
    roots: Vec<PathBuf>,
    observed_roots: Vec<crate::workspace::RootIdentity>,
    root_count: usize,
    hidden: bool,
    ignored: bool,
    /// Whether keys type into the search at the top.
    searching: bool,
    /// The row under the last pointer press, read before focus moved the tree.
    clicked: Option<ExplorerNodeId>,
    follow: Option<PathBuf>,
    next_load: Option<ExplorerNodeId>,
    watch: ExplorerWatchService,
    watch_generation: Option<ExplorerWatchGeneration>,
    watch_observation: Option<(u64, u64)>,
    watch_visible: bool,
    watch_reload: VecDeque<PathBuf>,
}
impl ExplorerSession {
    pub(in crate::editor::session) fn new(watch: ExplorerWatchService) -> Self {
        Self {
            primary: ExplorerViewState {
                tree: ExplorerState::default(),
                scroll: 0,
            },
            cloned_views: BTreeMap::new(),
            active_view: PRIMARY_EXPLORER_VIEW,
            next_view: PRIMARY_EXPLORER_VIEW.0,
            view_identity: Arc::new(()),
            view_source: Arc::new(()),
            mutation: None,
            rename: None,
            open: None,
            clipboard: None,
            directory: DirectoryService::default(),
            pending: None,
            epoch: Arc::new(()),
            roots: Vec::new(),
            observed_roots: Vec::new(),
            root_count: 0,
            hidden: false,
            ignored: false,
            searching: false,
            clicked: None,
            follow: None,
            next_load: None,
            watch,
            watch_generation: None,
            watch_observation: None,
            watch_visible: false,
            watch_reload: VecDeque::new(),
        }
    }
    pub(in crate::editor::session) fn tree(&self) -> &ExplorerState {
        self.tree_for(self.active_view)
            .expect("active explorer view is owned")
    }
    pub(in crate::editor::session) fn view_ids(&self) -> impl Iterator<Item = ExplorerViewId> + '_ {
        std::iter::once(PRIMARY_EXPLORER_VIEW).chain(self.cloned_views.keys().copied())
    }
    fn tree_mut(&mut self) -> &mut ExplorerState {
        if self.active_view == PRIMARY_EXPLORER_VIEW {
            &mut self.primary.tree
        } else {
            &mut self
                .cloned_views
                .get_mut(&self.active_view)
                .expect("active explorer view is owned")
                .tree
        }
    }
    fn tree_for(&self, view: ExplorerViewId) -> Option<&ExplorerState> {
        if view == PRIMARY_EXPLORER_VIEW {
            Some(&self.primary.tree)
        } else {
            self.cloned_views.get(&view).map(|view| &view.tree)
        }
    }
    fn tree_for_mut(&mut self, view: ExplorerViewId) -> Option<&mut ExplorerState> {
        if view == PRIMARY_EXPLORER_VIEW {
            Some(&mut self.primary.tree)
        } else {
            self.cloned_views.get_mut(&view).map(|view| &mut view.tree)
        }
    }
    fn synchronize_tree_data(&mut self, source: ExplorerViewId) {
        let Some(tree) = self.tree_for(source).cloned() else {
            return;
        };
        if source != PRIMARY_EXPLORER_VIEW {
            self.primary.tree.synchronize_data_from(&tree);
        }
        for (id, view) in &mut self.cloned_views {
            if *id != source {
                view.tree.synchronize_data_from(&tree);
            }
        }
        self.view_source = Arc::new(());
    }
    fn mark_view_changed(&mut self) {
        self.view_source = Arc::new(());
    }
    fn active_scroll(&self) -> usize {
        if self.active_view == PRIMARY_EXPLORER_VIEW {
            self.primary.scroll
        } else {
            self.cloned_views
                .get(&self.active_view)
                .expect("active explorer view is owned")
                .scroll
        }
    }
    fn set_active_scroll(&mut self, scroll: usize) {
        let target = if self.active_view == PRIMARY_EXPLORER_VIEW {
            &mut self.primary.scroll
        } else {
            &mut self
                .cloned_views
                .get_mut(&self.active_view)
                .expect("active explorer view is owned")
                .scroll
        };
        if *target != scroll {
            *target = scroll;
            self.mark_view_changed();
        }
    }
    fn watch_projection_generation(&self) -> u64 {
        self.cloned_views.values().fold(
            self.primary.tree.projection_generation(),
            |generation, view| generation.saturating_add(view.tree.projection_generation()),
        )
    }
    fn watched_directories(&self) -> Vec<PathBuf> {
        self.cloned_views
            .values()
            .flat_map(|view| view.tree.watched_directories())
            .chain(self.primary.tree.watched_directories())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn any_view_expanded(&self, id: ExplorerNodeId) -> bool {
        self.primary.tree.is_expanded(id)
            || self
                .cloned_views
                .values()
                .any(|view| view.tree.is_expanded(id))
    }
    pub(in crate::editor::session) fn remove_view(
        &mut self,
        view: ExplorerViewId,
    ) -> Result<(), &'static str> {
        if view == PRIMARY_EXPLORER_VIEW || self.cloned_views.remove(&view).is_none() {
            return Err("explorer view is unavailable");
        }
        if self.active_view == view {
            self.active_view = PRIMARY_EXPLORER_VIEW;
        }
        self.mark_view_changed();
        Ok(())
    }
    pub(in crate::editor::session) fn activate_view(
        &mut self,
        view: ExplorerViewId,
    ) -> Result<(), &'static str> {
        if self.tree_for(view).is_none() {
            return Err("explorer view is unavailable");
        }
        if self.active_view != view {
            self.active_view = view;
            self.mark_view_changed();
        }
        Ok(())
    }
    pub(in crate::editor::session) fn prepare_clone(
        &self,
        source: ExplorerViewId,
    ) -> Result<ExplorerCloneCandidate, &'static str> {
        if self.cloned_views.len().saturating_add(1) >= crate::UNIVERSAL_LAYOUT_WINDOW_CEILING {
            return Err("explorer view ceiling reached");
        }
        let id = ExplorerViewId(
            self.next_view
                .checked_add(1)
                .ok_or("explorer view identity exhausted")?,
        );
        let source_view = if source == PRIMARY_EXPLORER_VIEW {
            &self.primary
        } else {
            self.cloned_views
                .get(&source)
                .ok_or("explorer clone source is unavailable")?
        };
        Ok(ExplorerCloneCandidate {
            identity: self.view_identity.clone(),
            source: self.view_source.clone(),
            id,
            view: ExplorerViewState {
                tree: source_view.tree.clone_view(),
                scroll: source_view.scroll,
            },
        })
    }
    pub(in crate::editor::session) fn check_clone(
        &self,
        candidate: &ExplorerCloneCandidate,
    ) -> Result<(), &'static str> {
        if !Arc::ptr_eq(&candidate.identity, &self.view_identity)
            || !Arc::ptr_eq(&candidate.source, &self.view_source)
            || candidate.id.0
                != self
                    .next_view
                    .checked_add(1)
                    .ok_or("explorer view identity exhausted")?
            || self.cloned_views.contains_key(&candidate.id)
        {
            return Err("explorer clone source is stale or foreign");
        }
        Ok(())
    }
    pub(in crate::editor::session) fn accept_clone(
        &mut self,
        candidate: ExplorerCloneCandidate,
    ) -> Result<ExplorerViewId, &'static str> {
        self.check_clone(&candidate)?;
        self.cloned_views.insert(candidate.id, candidate.view);
        self.next_view = candidate.id.0;
        self.active_view = candidate.id;
        self.mark_view_changed();
        Ok(candidate.id)
    }
    #[cfg(test)]
    fn views_share_data(&self, first: ExplorerViewId, second: ExplorerViewId) -> bool {
        self.tree_for(first)
            .zip(self.tree_for(second))
            .is_some_and(|(first, second)| first.shares_data_with(second))
    }
    pub(in crate::editor::session) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::editor::session) fn reset_roots(
        &mut self,
        roots: &[WorkspaceRoot],
        revision: u64,
    ) {
        self.epoch = Arc::new(());
        self.observed_roots.clear();
        self.next_load = None;
        self.follow = None;
        self.root_count = roots.len();
        self.roots = roots
            .iter()
            .map(|r| r.identity.canonical_path.clone())
            .collect();
        self.primary.tree.reset_roots(roots, revision);
        self.synchronize_tree_data(PRIMARY_EXPLORER_VIEW);
        self.mark_view_changed();
        self.watch_observation = None;
        self.watch_reload.clear();
    }
    pub(in crate::editor::session) fn select(&mut self, id: ExplorerNodeId) {
        self.tree_mut().select(id);
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn select_first_child(&mut self) {
        self.tree_mut().select_first_child();
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn toggle_loaded(&mut self, id: ExplorerNodeId) {
        self.tree_mut().toggle_loaded(id);
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn navigate(&mut self, delta: isize) {
        self.tree_mut().move_selection(delta);
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn parent(&mut self) {
        self.tree_mut().collapse_or_parent();
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn start_search(&mut self) {
        self.searching = true;
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn set_clicked(&mut self, clicked: Option<ExplorerNodeId>) {
        self.clicked = clicked;
    }
    pub(in crate::editor::session) fn take_clicked(&mut self) -> Option<ExplorerNodeId> {
        self.clicked.take()
    }
    /// Whether this view's explorer has a query, or the user is typing one.
    pub(in crate::editor::session) fn searching(&self) -> bool {
        self.searching
    }
    /// Filter by `query` and select the first match.
    pub(in crate::editor::session) fn set_search(&mut self, query: String) {
        self.tree_mut().set_filter(query);
        self.select_search_match();
    }
    /// Select the first match unless a match is selected; matches turn up as the
    /// background reads finish.
    pub(in crate::editor::session) fn select_search_match(&mut self) {
        let tree = self.tree();
        let needle = tree.filter().to_lowercase();
        let matches = |node: &crate::workspace::ExplorerNode| {
            node.kind != ExplorerKind::Root && node.name.to_lowercase().contains(&needle)
        };
        if needle.is_empty() || tree.selected_node().is_some_and(matches) {
            return;
        }
        let found = tree
            .visible_ids()
            .iter()
            .find(|id| tree.node(**id).is_some_and(matches))
            .copied();
        if let Some(found) = found {
            self.tree_mut().select(found);
        }
        self.mark_view_changed();
    }
    /// Stop typing; `keep` leaves the tree filtered so `j`/`k` and enter work on the matches.
    pub(in crate::editor::session) fn stop_search(&mut self, keep: bool) {
        self.searching = false;
        if !keep {
            self.tree_mut().set_filter("");
        }
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn toggle_ignored(&mut self) {
        self.ignored = !self.ignored;
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn load(
        &mut self,
        node: ExplorerNodeId,
        scope: WorkspaceVersion,
        configuration: u64,
        maximum: usize,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            self.next_load = Some(node);
            return Ok(());
        }
        let source_view = self.active_view;
        let Some(path) = self.tree().node(node).map(|node| node.path.clone()) else {
            return Ok(());
        };
        let generation = self
            .directory
            .list(
                path,
                self.roots.clone(),
                self.tree().root_revision(),
                self.hidden,
                self.ignored,
                maximum,
            )
            .map_err(|e| e.to_string())?;
        self.tree_mut().begin_load(node, generation);
        self.synchronize_tree_data(source_view);
        self.mark_view_changed();
        self.pending = Some((
            source_view,
            node,
            generation,
            self.tree().root_revision(),
            scope,
            configuration,
            self.epoch.clone(),
        ));
        Ok(())
    }
    pub(in crate::editor::session) fn poll(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Option<Result<(), String>> {
        let result = self.directory.poll_result()?;
        let (source_view, node, generation, revision, expected, policy, epoch) =
            self.pending.take()?;
        if expected != scope || policy != configuration || !Arc::ptr_eq(&epoch, &self.epoch) {
            // Only retire the old load in its original tree; IDs can be reused after reset.
            if revision == self.tree().root_revision() {
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.set_error(node, generation);
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();
            }
            return Some(Ok(()));
        }
        match result {
            Ok((listing, identities))
                if listing.generation == generation
                    && listing.root_revision == revision
                    && revision == self.tree().root_revision() =>
            {
                for identity in identities {
                    self.observed_roots
                        .retain(|old| old.canonical_path != identity.canonical_path);
                    self.observed_roots.push(identity);
                }
                let children = listing
                    .entries
                    .into_iter()
                    .map(|entry| ExplorerChild {
                        path: entry.path,
                        name: entry.name,
                        kind: match entry.kind {
                            DirectoryEntryKind::Directory => ExplorerKind::Directory,
                            DirectoryEntryKind::File => ExplorerKind::File,
                            DirectoryEntryKind::Symlink => ExplorerKind::Symlink,
                        },
                    })
                    .collect();
                let root_selected = self
                    .tree_for(source_view)
                    .is_some_and(|tree| tree.selected_id() == Some(node))
                    && self
                        .tree_for(source_view)
                        .and_then(|tree| tree.node(node))
                        .is_some_and(|node| node.kind == ExplorerKind::Root);
                let select_first_child = root_selected && self.root_count == 1;
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.apply_load(node, generation, revision, children);
                    if select_first_child {
                        tree.select_first_child();
                    }
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();
                if root_selected && self.root_count == 1 && self.active_view == source_view {
                }
                Some(Ok(()))
            }
            Ok(_) => {
                if revision == self.tree().root_revision() {
                    if let Some(tree) = self.tree_for_mut(source_view) {
                        tree.set_error(node, generation);
                    }
                    self.synchronize_tree_data(source_view);
                    self.mark_view_changed();
                }
                Some(Ok(()))
            }
            Err(error) => {
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.set_error(node, generation);
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();
                Some(Err(error.to_string()))
            }
        }
    }
    pub(in crate::editor::session) fn follow(&mut self, path: PathBuf) {
        self.follow = Some(path);
        self.mark_view_changed();
    }
    pub(in crate::editor::session) fn continue_follow(
        &mut self,
    ) -> Result<Option<ExplorerNodeId>, String> {
        if self.pending() {
            return Ok(None);
        }
        let Some(target) = self.follow.clone() else {
            return Ok(None);
        };
        if let Some(id) = self.tree().id_for_path(&target) {
            self.tree_mut().select(id);
            self.mark_view_changed();
            self.follow = None;
            return Ok(None);
        }
        let id = target
            .ancestors()
            .skip(1)
            .find_map(|path| self.tree().id_for_path(path));
        let Some(id) = id else {
            self.follow = None;
            return Err("active file has no explorer root".into());
        };
        self.tree_mut().select(id);
        self.mark_view_changed();
        if self
            .tree()
            .node(id)
            .is_some_and(|node| node.load == ExplorerLoadState::Loaded)
        {
            self.follow = None;
            return Err("active file is absent from the current explorer projection".into());
        }
        Ok(Some(id))
    }
    pub(in crate::editor::session) fn reconcile_watch(
        &mut self,
        visible: bool,
    ) -> Result<(), String> {
        if self.watch_visible && !visible {
            self.epoch = Arc::new(());
            self.follow = None;
            self.next_load = None;
        }
        self.watch_visible = visible;
        if !visible {
            if self.watch_generation.take().is_some() {
                self.watch.cancel();
            }
            self.watch_observation = None;
            self.watch_reload.clear();
            return Ok(());
        }
        let observation = (
            self.watch_projection_generation(),
            self.tree().root_revision(),
        );
        if self.watch_observation == Some(observation) {
            return Ok(());
        }
        self.watch_observation = Some(observation);
        match self.watch.reconcile(
            observation.1,
            self.roots.clone(),
            self.watched_directories(),
        ) {
            Ok(generation) => {
                self.watch_generation = generation;
                Ok(())
            }
            Err(_) => {
                self.watch_generation = None;
                Err("explorer watch reconciliation failed".into())
            }
        }
    }
    pub(in crate::editor::session) fn accept_watch(
        &mut self,
        batch: ExplorerWatchBatch,
        visible: bool,
    ) {
        if !visible
            || Some(batch.generation) != self.watch_generation
            || batch.root_revision != self.tree().root_revision()
        {
            return;
        }
        for path in batch.directories {
            if !self.watch_reload.contains(&path) {
                self.watch_reload.push_back(path);
            }
        }
    }
    pub(in crate::editor::session) fn poll_watch(&mut self, visible: bool) -> Result<(), String> {
        for event in self.watch.poll() {
            match event {
                ExplorerWatchEvent::Batch(batch) => self.accept_watch(batch, visible),
                ExplorerWatchEvent::Failed {
                    generation,
                    root_revision,
                } if Some(generation) == self.watch_generation
                    && root_revision == self.tree().root_revision() =>
                {
                    self.watch_generation = None;
                    return Err("explorer watch worker rejected the active set".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(in crate::editor::session) fn next_reload(&mut self) -> Option<ExplorerNodeId> {
        if self.pending() {
            return None;
        }
        if let Some(node) = self.next_load.take() {
            return Some(node);
        }
        while let Some(path) = self.watch_reload.pop_front() {
            let Some(id) = self.tree().id_for_path(&path) else {
                continue;
            };
            if self.tree().node(id).is_some_and(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && self.any_view_expanded(id)
                    && node.load == ExplorerLoadState::Loaded
            }) {
                return Some(id);
            }
        }
        // A search reads the folders not read yet, one at a time.
        if self.tree().filter().is_empty() {
            return None;
        }
        self.tree().unloaded_directory(SEARCH_ENTRY_LIMIT)
    }
    pub(in crate::editor::session) fn reset_watch_observation(&mut self) {
        self.watch_observation = None;
    }
    pub(in crate::editor::session) fn projection_for(
        &mut self,
        view: ExplorerViewId,
        config: &crate::config::ExplorerConfig,
        body: crate::ui::window::WindowRect,
        git_status: &BTreeMap<PathBuf, crate::workspace::git::GitFileStatus>,
        title: String,
        root_label: &str,
    ) -> Result<ExplorerUiState, &'static str> {
        if self.tree_for(view).is_none() {
            return Err("explorer view is unavailable");
        }
        let active = self.active_view;
        self.active_view = view;
        let projection = self.projection(config, body, git_status, title, root_label);
        self.active_view = active;
        Ok(projection)
    }
    pub(in crate::editor::session) fn projection(
        &mut self,
        config: &crate::config::ExplorerConfig,
        body: crate::ui::window::WindowRect,
        git_status: &BTreeMap<PathBuf, crate::workspace::git::GitFileStatus>,
        title: String,
        root_label: &str,
    ) -> ExplorerUiState {
        self.tree_mut().ensure_selection();
        let tree = self.tree();
        let visible_ids = tree.visible_ids();
        let total = visible_ids.len();
        let selected_id = tree.selected_id();
        let selected = selected_id
            .and_then(|id| tree.visible_index(id));
        let maximum = usize::from(body.height);
        let previous_scroll = self.active_scroll().min(total.saturating_sub(maximum));
        let scroll = selected.map_or(previous_scroll, |selected| {
            if selected < previous_scroll {
                selected
            } else if selected >= previous_scroll.saturating_add(maximum) {
                selected.saturating_sub(maximum.saturating_sub(1))
            } else {
                previous_scroll
            }
        });
        let rich_icons = config.icon_mode == crate::config::ExplorerIconMode::NerdFont;
        let glyphs = config.active_glyphs();
        let rows = visible_ids
            .iter()
            .skip(scroll)
            .take(maximum)
            .filter_map(|id| tree.node(*id))
            .map(|node| {
                let lineage = tree.tree_lineage(node.id);
                let mut tree_prefix = String::new();
                for (index, is_last) in lineage.iter().enumerate() {
                    let final_segment = index + 1 == lineage.len();
                    tree_prefix.push_str(if final_segment {
                        if *is_last {
                            &glyphs.tree_last
                        } else {
                            &glyphs.tree_middle
                        }
                    } else if *is_last {
                        &glyphs.tree_space
                    } else {
                        &glyphs.tree_vertical
                    });
                }
                let git_status = git_status.get(&node.path);
                let name_role = if git_status.is_some() {
                    ExplorerSegmentRole::Git
                } else {
                    match node.kind {
                        ExplorerKind::Root => ExplorerSegmentRole::IconPurple,
                        ExplorerKind::Directory => ExplorerSegmentRole::Directory,
                        ExplorerKind::File => ExplorerSegmentRole::File,
                        ExplorerKind::Symlink => ExplorerSegmentRole::Symlink,
                    }
                };
                let rich_icon = if rich_icons {
                    match node.kind {
                        ExplorerKind::Directory => crate::files::directory_icon(&node.name),
                        ExplorerKind::File => crate::files::file_icon(&node.name),
                        ExplorerKind::Root | ExplorerKind::Symlink => None,
                    }
                } else {
                    None
                };
                let (icon, icon_role) = match (node.kind, node.load, tree.is_expanded(node.id)) {
                    (
                        ExplorerKind::Root | ExplorerKind::Directory,
                        ExplorerLoadState::Loading(_),
                        _,
                    ) => (glyphs.loading.clone(), ExplorerSegmentRole::Loading),
                    _ if rich_icon.is_some() => {
                        let icon = rich_icon.expect("checked rich explorer icon");
                        (icon.glyph.into(), explorer_icon_role(icon.color))
                    }
                    (ExplorerKind::Root | ExplorerKind::Directory, _, true) => {
                        (glyphs.directory_open.clone(), ExplorerSegmentRole::IconBlue)
                    }
                    (ExplorerKind::Root | ExplorerKind::Directory, _, false) => {
                        (glyphs.directory.clone(), ExplorerSegmentRole::IconBlue)
                    }
                    (ExplorerKind::File, _, _) => (glyphs.file.clone(), ExplorerSegmentRole::Icon),
                    (ExplorerKind::Symlink, _, _) => {
                        (glyphs.symlink.clone(), ExplorerSegmentRole::Icon)
                    }
                };
                let mut segments = Vec::with_capacity(4);
                if !tree_prefix.is_empty() {
                    segments.push(ExplorerUiSegment {
                        text: tree_prefix,
                        role: ExplorerSegmentRole::Tree,
                    });
                }
                segments.push(ExplorerUiSegment {
                    text: icon,
                    role: icon_role,
                });
                segments.push(ExplorerUiSegment {
                    text: glyphs.icon_spacing.clone(),
                    role: ExplorerSegmentRole::Tree,
                });
                segments.push(ExplorerUiSegment {
                    text: if node.kind == ExplorerKind::Root && self.root_count == 1 {
                        root_label.to_owned()
                    } else {
                        node.name.clone()
                    },
                    role: name_role,
                });
                let right_segments = git_status
                    .map(|status| ExplorerUiSegment {
                        text: match status {
                            crate::workspace::git::GitFileStatus::Untracked => "?",
                            crate::workspace::git::GitFileStatus::Added => "A",
                            crate::workspace::git::GitFileStatus::Modified => "M",
                            crate::workspace::git::GitFileStatus::Deleted => "D",
                            crate::workspace::git::GitFileStatus::Renamed => "R",
                            crate::workspace::git::GitFileStatus::Conflicted => "!",
                        }
                        .into(),
                        role: ExplorerSegmentRole::Git,
                    })
                    .into_iter()
                    .collect();
                ExplorerUiRow {
                    node_id: node.id.0,
                    selected: selected_id == Some(node.id),
                    segments,
                    right_segments,
                }
            })
            .collect();
        let needle = tree.filter().to_lowercase();
        let matches = if needle.is_empty() {
            total
        } else {
            visible_ids
                .iter()
                .filter_map(|id| tree.node(*id))
                .filter(|node| {
                    node.kind != ExplorerKind::Root && node.name.to_lowercase().contains(&needle)
                })
                .count()
        };
        self.set_active_scroll(scroll);
        ExplorerUiState {
            title,
            total,
            matches,
            query: self.tree().filter().to_owned(),
            searching: self.searching,
            rows,
            scroll,
            loading: self.pending.is_some(),
        }
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn watch_generation(&self) -> Option<ExplorerWatchGeneration> {
        self.watch_generation
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn watch_reconciles(&self) -> u64 {
        self.watch.reconcile_count()
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn watch_queue_empty(&self) -> bool {
        self.watch_reload.is_empty()
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn following(&self) -> bool {
        self.follow.is_some()
    }
}
pub(in crate::editor::session) fn explorer_icon_role(color: crate::files::ExplorerIconColor) -> ExplorerSegmentRole {
    match color {
        crate::files::ExplorerIconColor::Azure => ExplorerSegmentRole::IconAzure,
        crate::files::ExplorerIconColor::Blue => ExplorerSegmentRole::IconBlue,
        crate::files::ExplorerIconColor::Cyan => ExplorerSegmentRole::IconCyan,
        crate::files::ExplorerIconColor::Green => ExplorerSegmentRole::IconGreen,
        crate::files::ExplorerIconColor::Grey => ExplorerSegmentRole::IconGrey,
        crate::files::ExplorerIconColor::Orange => ExplorerSegmentRole::IconOrange,
        crate::files::ExplorerIconColor::Purple => ExplorerSegmentRole::IconPurple,
        crate::files::ExplorerIconColor::Red => ExplorerSegmentRole::IconRed,
        crate::files::ExplorerIconColor::Yellow => ExplorerSegmentRole::IconYellow,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn fixture(root: &std::path::Path) -> (ExplorerSession, WorkspaceVersion) {
        let identity = crate::workspace::RootIdentity::capture(root).unwrap();
        let state = crate::workspace::WorkspaceState::new(
            crate::workspace::WorkspaceId(1),
            [("root".into(), identity)],
        )
        .unwrap();
        let workspace = crate::editor::session::workspace::WorkspaceSession::new(
            state,
            root.join("trust.json"),
            1,
            false,
            false,
            1,
        );
        let mut owner = ExplorerSession::new(
            ExplorerWatchService::start(crate::files::ExplorerWatchPolicy::default())
                .unwrap(),
        );
        owner.reset_roots(workspace.roots(), workspace.root_revision());
        (owner, workspace.version())
    }
    fn drain(owner: &mut ExplorerSession, scope: WorkspaceVersion) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while owner.pending() {
            owner.poll(scope.clone(), 1);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn closing_and_reopening_retires_old_directory_results_before_tree_projection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("one.txt");
        std::fs::write(&path, "one").unwrap();
        let (mut owner, source) = fixture(root.path());
        let node = owner.tree().selected_id().unwrap();
        owner.reconcile_watch(true).unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        owner.reconcile_watch(false).unwrap();
        drain(&mut owner, source.clone());
        assert!(owner.tree().id_for_path(&path).is_none());
        assert!(owner.next_reload().is_none());
        owner.reconcile_watch(true).unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        drain(&mut owner, source);
        assert!(owner.tree().id_for_path(&path).is_some());
    }
    #[test]
    fn retired_directory_policy_has_terminal_dispositions() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let (mut owner, source) = fixture(root.path());
        let node = owner.tree().selected_id().unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        let wait = |owner: &mut ExplorerSession| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while owner.pending() {
                owner.poll(source.clone(), 2);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        wait(&mut owner);
        assert_eq!(
            owner.tree().node(node).unwrap().load,
            ExplorerLoadState::Error
        );
        owner.load(node, source.clone(), 2, 100).unwrap();
        wait(&mut owner);
        assert_eq!(
            owner.tree().node(node).unwrap().load,
            ExplorerLoadState::Loaded
        );
    }
    #[test]
    fn cloned_explorer_views_share_tree_data_but_own_projection_state() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(root.path().join("file.txt"), "file").unwrap();
        std::fs::write(directory.join("nested.txt"), "nested").unwrap();
        let (mut owner, source) = fixture(root.path());
        let root_node = owner.tree().selected_id().unwrap();
        owner.load(root_node, source.clone(), 1, 100).unwrap();
        drain(&mut owner, source.clone());

        let asynchronous_stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        std::fs::write(root.path().join("arrived-later.txt"), "late").unwrap();
        owner.load(root_node, source.clone(), 1, 100).unwrap();
        drain(&mut owner, source);
        assert_eq!(
            owner.accept_clone(asynchronous_stale),
            Err("explorer clone source is stale or foreign")
        );

        let stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        owner.tree_mut().move_selection(1);
        owner.mark_view_changed();
        assert_eq!(
            owner.accept_clone(stale),
            Err("explorer clone source is stale or foreign")
        );

        let primary_selection = owner.tree().selected_id();
        let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        let clone = candidate.id();
        assert_eq!(owner.accept_clone(candidate), Ok(clone));
        assert!(owner.views_share_data(PRIMARY_EXPLORER_VIEW, clone));
        assert_eq!(
            owner.tree_for(clone).unwrap().selected_id(),
            primary_selection
        );

        owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
        let focus_stale = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        owner.activate_view(clone).unwrap();
        assert_eq!(
            owner.accept_clone(focus_stale),
            Err("explorer clone source is stale or foreign")
        );

        owner.activate_view(PRIMARY_EXPLORER_VIEW).unwrap();
        owner.tree_mut().move_selection(-1);
        owner.mark_view_changed();
        let moved_primary = owner.tree().selected_id();
        assert_ne!(moved_primary, primary_selection);
        assert_eq!(
            owner.tree_for(clone).unwrap().selected_id(),
            primary_selection
        );

        owner.activate_view(clone).unwrap();
        owner.tree_mut().set_filter("nested");
        owner.mark_view_changed();
        assert_eq!(owner.tree().filter(), "nested");
        assert_eq!(owner.tree_for(PRIMARY_EXPLORER_VIEW).unwrap().filter(), "");
        assert!(owner.views_share_data(PRIMARY_EXPLORER_VIEW, clone));

        owner.select(root_node);
        owner.toggle_loaded(root_node);
        assert!(!owner.tree().is_expanded(root_node));
        assert!(
            owner
                .tree_for(PRIMARY_EXPLORER_VIEW)
                .unwrap()
                .is_expanded(root_node)
        );
    }

    #[test]
    fn explorer_clone_rejects_foreign_exhausted_and_over_ceiling_sources_without_publication() {
        let root = tempfile::tempdir().unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let (mut owner, _) = fixture(root.path());
        let (other, _) = fixture(other_root.path());
        let foreign = other.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
        assert_eq!(
            owner.accept_clone(foreign),
            Err("explorer clone source is stale or foreign")
        );
        assert_eq!(owner.cloned_views.len(), 0);

        for expected in 2..=crate::UNIVERSAL_LAYOUT_WINDOW_CEILING as u64 {
            let candidate = owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap();
            assert_eq!(candidate.id(), ExplorerViewId(expected));
            owner.accept_clone(candidate).unwrap();
        }
        assert_eq!(
            owner.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap_err(),
            "explorer view ceiling reached"
        );
        assert_eq!(
            owner.cloned_views.len().saturating_add(1),
            crate::UNIVERSAL_LAYOUT_WINDOW_CEILING
        );

        let (mut exhausted, _) = fixture(root.path());
        exhausted.next_view = u64::MAX;
        assert_eq!(
            exhausted.prepare_clone(PRIMARY_EXPLORER_VIEW).unwrap_err(),
            "explorer view identity exhausted"
        );
        assert!(exhausted.cloned_views.is_empty());
    }

    #[test]
    #[ignore = "explicit explorer ownership cost probe"]
    fn explorer_owner_cost_probe() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..40 {
            std::fs::write(root.path().join(format!("file-{n:02}.txt")), "file").unwrap();
        }
        let (mut owner, source) = fixture(root.path());
        let node = owner.tree().selected_id().unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        drain(&mut owner, source);
        let config = crate::config::load_bundled_config();
        let git = BTreeMap::new();
        let body = crate::ui::window::WindowRect {
            x: 0,
            y: 3,
            width: 30,
            height: 20,
        };
        let before = (
            crate::ui::window::WindowGraph::test_clone_count(),
            crate::editor::EditorState::test_clone_count(),
            crate::editor::text::TextBuffer::test_materializations(),
        );
        eprintln!("DUST_ALLOC_START");
        let start = Instant::now();
        let mut rows = 0;
        for _ in 0..100 {
            rows += std::hint::black_box(owner.projection(
                &config.config.ui.explorer,
                body,
                &git,
                "Files".into(),
                "root",
            ))
            .rows
            .len();
        }
        let elapsed = start.elapsed();
        eprintln!("DUST_ALLOC_END");
        let after = crate::editor::text::TextBuffer::test_materializations();
        println!(
            "case=explorer_projection iterations=100 rows_materialized={rows} elapsed_ns={} graph_copies={} editor_copies={} text_decodes={} text_displays={}",
            elapsed.as_nanos(),
            crate::ui::window::WindowGraph::test_clone_count() - before.0,
            crate::editor::EditorState::test_clone_count() - before.1,
            after.0 - before.2.0,
            after.1 - before.2.1
        );
        assert_eq!(rows, 2000);
    }
}
