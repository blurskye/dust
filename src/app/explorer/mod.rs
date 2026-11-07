//! Explorer tree, directory lane, watch and viewport ownership.
mod intent;
use crate::app::explorer::tree::ExplorerChild;
use crate::app::explorer::tree::ExplorerKind;
use crate::app::explorer::tree::ExplorerLoadState;
use crate::app::explorer::tree::ExplorerNodeId;
use crate::app::explorer::tree::ExplorerState;
use crate::app::workspace::WorkspaceVersion;
use crate::files::DirectoryEntryKind;
use crate::files::DirectoryWorker;
use crate::files::ExplorerWatchBatch;
use crate::files::ExplorerWatchEvent;
use crate::files::ExplorerWatchId;
use crate::files::ExplorerWatchWorker;
use crate::ui::ExplorerSegmentRole;
use crate::ui::ExplorerUiRow;
use crate::ui::ExplorerUiSegment;
use crate::ui::ExplorerUiState;
use crate::workspace::WorkspaceRoot;
pub(in crate::app) use intent::MutationContext;
pub(in crate::app) use intent::MutationUpdate;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::app) struct ExplorerViewId(pub u64);
pub(in crate::app) const PRIMARY_EXPLORER_VIEW: ExplorerViewId = ExplorerViewId(1);

#[derive(Clone, Debug)]
struct ExplorerViewState {
    tree: ExplorerState,
    scroll: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) struct DirectoryLoad {
    view: ExplorerViewId,
    node: ExplorerNodeId,
    expand: bool,
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
pub(in crate::app) struct ExplorerCloneCandidate {
    identity: Arc<()>,
    source: Arc<()>,
    id: ExplorerViewId,
    view: ExplorerViewState,
}
impl ExplorerCloneCandidate {
    pub(in crate::app) fn id(&self) -> ExplorerViewId {
        self.id
    }
}

/// How many entries a search reads at most, so a huge tree does not stall it.
const SEARCH_ENTRY_LIMIT: usize = 50_000;

pub(in crate::app) struct Explorer {
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
    directory: DirectoryWorker,
    maximum_entries: BTreeMap<ExplorerNodeId, usize>,
    pending: Option<PendingDirectoryLoad>,
    epoch: Arc<()>,
    roots: Vec<PathBuf>,
    observed_roots: Vec<crate::files::RootIdentity>,
    root_count: usize,
    hidden: bool,
    ignored: bool,
    /// Whether keys type into the search at the top.
    searching: bool,
    /// The row under the last pointer press, read before focus moved the tree.
    clicked: Option<ExplorerNodeId>,
    follow: Option<(ExplorerViewId, PathBuf)>,
    next_loads: VecDeque<DirectoryLoad>,
    watch: ExplorerWatchWorker,
    watch_stream_id: Option<ExplorerWatchId>,
    watch_observation: Option<(u64, u64, Vec<PathBuf>)>,
    watch_visible: bool,
    watch_reload: VecDeque<PathBuf>,
}
impl Explorer {
    pub(in crate::app) fn new(watch: ExplorerWatchWorker) -> Self {
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
            directory: DirectoryWorker::default(),
            maximum_entries: BTreeMap::new(),
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
            next_loads: VecDeque::new(),
            watch,
            watch_stream_id: None,
            watch_observation: None,
            watch_visible: false,
            watch_reload: VecDeque::new(),
        }
    }
    pub(in crate::app) fn tree(&self) -> &ExplorerState {
        self.tree_for(self.active_view)
            .expect("active explorer view is owned")
    }
    pub(in crate::app) fn view_ids(&self) -> impl Iterator<Item = ExplorerViewId> + '_ {
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
    fn watch_projection_revision(&self) -> u64 {
        self.cloned_views
            .values()
            .fold(self.primary.tree.projection_revision(), |revision, view| {
                revision.saturating_add(view.tree.projection_revision())
            })
    }
    pub(in crate::app) fn watched_directories(&self) -> Vec<PathBuf> {
        self.cloned_views
            .values()
            .flat_map(|view| view.tree.watched_directories())
            .chain(self.primary.tree.watched_directories())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn any_view_watches_directory(&self, id: ExplorerNodeId) -> bool {
        self.primary.tree.is_watched_directory(id)
            || self
                .cloned_views
                .values()
                .any(|view| view.tree.is_watched_directory(id))
    }
    pub(in crate::app) fn remove_view(&mut self, view: ExplorerViewId) -> Result<(), &'static str> {
        if view == PRIMARY_EXPLORER_VIEW || self.cloned_views.remove(&view).is_none() {
            return Err("explorer view is unavailable");
        }
        if self.active_view == view {
            self.active_view = PRIMARY_EXPLORER_VIEW;
            self.searching = false;
        }
        self.next_loads.retain(|load| load.view != view);
        if self
            .follow
            .as_ref()
            .is_some_and(|(source, _)| *source == view)
        {
            self.follow = None;
        }
        self.mark_view_changed();
        Ok(())
    }
    pub(in crate::app) fn activate_view(
        &mut self,
        view: ExplorerViewId,
    ) -> Result<(), &'static str> {
        if self.tree_for(view).is_none() {
            return Err("explorer view is unavailable");
        }
        if self.active_view != view {
            self.active_view = view;
            self.searching = false;
            self.mark_view_changed();
        }
        Ok(())
    }
    pub(in crate::app) fn prepare_clone(
        &self,
        source: ExplorerViewId,
    ) -> Result<ExplorerCloneCandidate, &'static str> {
        if self.cloned_views.len().saturating_add(1) >= crate::windows::MAX_WINDOWS {
            return Err("explorer view limit reached");
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
    pub(in crate::app) fn check_clone(
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
    pub(in crate::app) fn accept_clone(
        &mut self,
        candidate: ExplorerCloneCandidate,
    ) -> Result<ExplorerViewId, &'static str> {
        self.check_clone(&candidate)?;
        self.cloned_views.insert(candidate.id, candidate.view);
        self.next_view = candidate.id.0;
        self.active_view = candidate.id;
        self.searching = false;
        self.mark_view_changed();
        Ok(candidate.id)
    }
    #[cfg(test)]
    fn views_share_data(&self, first: ExplorerViewId, second: ExplorerViewId) -> bool {
        self.tree_for(first)
            .zip(self.tree_for(second))
            .is_some_and(|(first, second)| first.shares_data_with(second))
    }
    pub(in crate::app) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::app) fn reset_roots(&mut self, roots: &[WorkspaceRoot], revision: u64) {
        self.epoch = Arc::new(());
        self.observed_roots.clear();
        self.next_loads.clear();
        self.follow = None;
        self.root_count = roots.len();
        self.roots = roots
            .iter()
            .map(|r| r.identity.canonical_path.clone())
            .collect();
        self.maximum_entries.clear();
        self.primary.tree.reset_roots(roots, revision);
        self.synchronize_tree_data(PRIMARY_EXPLORER_VIEW);
        self.mark_view_changed();
        self.watch_observation = None;
        self.watch_reload.clear();
    }
    pub(in crate::app) fn select(&mut self, id: ExplorerNodeId) {
        self.tree_mut().select(id);
        self.mark_view_changed();
    }
    pub(in crate::app) fn select_first_child(&mut self) {
        if let Some(id) = self.tree().selected_id()
            && !self.tree().is_expanded(id)
        {
            self.toggle_loaded(id);
        }
        self.tree_mut().select_first_child();
        self.mark_view_changed();
    }
    pub(in crate::app) fn toggle_loaded(&mut self, id: ExplorerNodeId) {
        let was_expanded = self.tree().is_expanded(id);
        self.tree_mut().toggle_loaded(id);
        if !was_expanded
            && self.tree().is_expanded(id)
            && let Some(node) = self.tree().node(id)
        {
            self.watch_reload.push_back(node.path.clone());
        }
        self.mark_view_changed();
    }
    pub(in crate::app) fn navigate(&mut self, delta: isize) {
        self.tree_mut().move_selection(delta);
        self.mark_view_changed();
    }
    pub(in crate::app) fn parent(&mut self) {
        self.tree_mut().collapse_or_parent();
        self.mark_view_changed();
    }
    pub(in crate::app) fn start_search(&mut self) {
        self.searching = true;
        self.mark_view_changed();
    }
    pub(in crate::app) fn set_clicked(&mut self, clicked: Option<ExplorerNodeId>) {
        self.clicked = clicked;
    }
    pub(in crate::app) fn take_clicked(&mut self) -> Option<ExplorerNodeId> {
        self.clicked.take()
    }
    /// Whether this view's explorer has a query, or the user is typing one.
    pub(in crate::app) fn searching(&self) -> bool {
        self.searching
    }
    /// Filter by `query` and select the first match.
    pub(in crate::app) fn set_search(&mut self, query: String) {
        self.tree_mut().set_filter(query);
        self.select_search_match();
    }
    /// Select the first match unless a match is selected; matches turn up as the
    /// background reads finish.
    pub(in crate::app) fn select_search_match(&mut self) {
        let tree = self.tree();
        let needle = tree.filter().to_lowercase();
        let matches = |node: &crate::app::explorer::tree::ExplorerNode| {
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
    pub(in crate::app) fn stop_search(&mut self, keep: bool) {
        self.searching = false;
        if !keep {
            self.tree_mut().set_filter("");
        }
        self.mark_view_changed();
    }
    pub(in crate::app) fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        self.mark_view_changed();
    }
    pub(in crate::app) fn toggle_ignored(&mut self) {
        self.ignored = !self.ignored;
        self.mark_view_changed();
    }
    fn load_request_for(
        &self,
        view: ExplorerViewId,
        node: ExplorerNodeId,
    ) -> Option<DirectoryLoad> {
        let tree = self.tree_for(view)?;
        let entry = tree.node(node)?;
        Some(DirectoryLoad {
            view,
            node,
            expand: tree.filter().is_empty()
                && !tree.is_expanded(node)
                && entry.load != ExplorerLoadState::Loaded,
        })
    }
    fn queue_load(&mut self, request: DirectoryLoad) {
        if let Some(queued) = self
            .next_loads
            .iter_mut()
            .find(|queued| queued.view == request.view && queued.node == request.node)
        {
            queued.expand |= request.expand;
        } else {
            self.next_loads.push_back(request);
        }
    }
    pub(in crate::app) fn load(
        &mut self,
        node: ExplorerNodeId,
        scope: WorkspaceVersion,
        configuration: u64,
        maximum: usize,
    ) -> Result<(), String> {
        let Some(request) = self.load_request_for(self.active_view, node) else {
            return Ok(());
        };
        self.load_request(request, scope, configuration, maximum)
    }
    pub(in crate::app) fn load_request(
        &mut self,
        request: DirectoryLoad,
        scope: WorkspaceVersion,
        configuration: u64,
        maximum: usize,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            self.queue_load(request);
            return Ok(());
        }
        let source_view = request.view;
        let node = request.node;
        let Some(tree) = self.tree_for(source_view) else {
            return Ok(());
        };
        let Some(path) = tree.node(node).map(|node| node.path.clone()) else {
            return Ok(());
        };
        let revision = tree.root_revision();
        let maximum = self.maximum_entries.get(&node).copied().unwrap_or(maximum);
        let request_id = self
            .directory
            .list(
                path,
                self.roots.clone(),
                revision,
                self.hidden,
                self.ignored,
                maximum,
            )
            .map_err(|e| e.to_string())?;
        if let Some(tree) = self.tree_for_mut(source_view) {
            let expanded = tree.is_expanded(node) || request.expand;
            tree.begin_load(node, request_id);
            if tree.is_expanded(node) != expanded {
                tree.toggle_loaded(node);
            }
        }
        self.synchronize_tree_data(source_view);
        self.mark_view_changed();
        self.pending = Some((
            source_view,
            node,
            request_id,
            revision,
            scope,
            configuration,
            self.epoch.clone(),
        ));
        Ok(())
    }
    pub(in crate::app) fn poll(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Option<Result<(), String>> {
        let result = self.directory.poll_result()?;
        let (source_view, node, request_id, revision, expected, policy, epoch) =
            self.pending.take()?;
        let source_view = if self.tree_for(source_view).is_some() {
            source_view
        } else {
            PRIMARY_EXPLORER_VIEW
        };
        if expected != scope || policy != configuration || !Arc::ptr_eq(&epoch, &self.epoch) {
            // Only cancel the old load in its original tree; IDs can be reused after reset.
            if revision == self.tree().root_revision() {
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.set_error(node, request_id);
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();
            }
            return Some(Ok(()));
        }
        match result {
            Ok((listing, identities))
                if listing.request_id == request_id
                    && listing.root_revision == revision
                    && revision == self.tree().root_revision() =>
            {
                for identity in identities {
                    self.observed_roots
                        .retain(|old| old.canonical_path != identity.canonical_path);
                    self.observed_roots.push(identity);
                }
                let truncated = listing.truncated;
                let count = listing.entries.len();
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
                let select_first_child = root_selected
                    && self.root_count == 1
                    && self
                        .tree_for(source_view)
                        .is_some_and(|tree| tree.is_expanded(node));
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.apply_load(node, request_id, revision, children);
                    if truncated && count >= 100_000 {
                        tree.begin_load(node, request_id);
                        tree.set_error(node, request_id);
                    }
                    if select_first_child {
                        tree.select_first_child();
                    }
                }
                if truncated && count < 100_000 {
                    self.maximum_entries
                        .insert(node, count.saturating_mul(2).clamp(1, 100_000));
                    self.queue_load(DirectoryLoad {
                        view: source_view,
                        node,
                        expand: false,
                    });
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();

                Some(if truncated && count >= 100_000 {
                    Err(
                        "directory listing exceeds 100000 entries; showing a partial listing"
                            .into(),
                    )
                } else {
                    Ok(())
                })
            }
            Ok(_) => {
                if revision == self.tree().root_revision() {
                    if let Some(tree) = self.tree_for_mut(source_view) {
                        tree.set_error(node, request_id);
                    }
                    self.synchronize_tree_data(source_view);
                    self.mark_view_changed();
                }
                Some(Ok(()))
            }
            Err(error) => {
                if let Some(tree) = self.tree_for_mut(source_view) {
                    tree.set_error(node, request_id);
                }
                self.synchronize_tree_data(source_view);
                self.mark_view_changed();
                Some(Err(error.to_string()))
            }
        }
    }
    pub(in crate::app) fn follow(&mut self, path: PathBuf) {
        self.follow = Some((self.active_view, path));
        self.mark_view_changed();
    }
    pub(in crate::app) fn continue_follow(&mut self) -> Result<Option<DirectoryLoad>, String> {
        if self.pending() || !self.next_loads.is_empty() {
            return Ok(None);
        }
        let Some((view, target)) = self.follow.clone() else {
            return Ok(None);
        };
        let Some(tree) = self.tree_for_mut(view) else {
            self.follow = None;
            return Ok(None);
        };
        if let Some(id) = tree.id_for_path(&target) {
            let ancestors = target
                .ancestors()
                .skip(1)
                .filter_map(|path| tree.id_for_path(path))
                .collect::<Vec<_>>();
            tree.set_filter("");
            for ancestor in ancestors {
                if !tree.is_expanded(ancestor) {
                    tree.toggle_loaded(ancestor);
                }
            }
            tree.select(id);
            self.mark_view_changed();
            self.follow = None;
            return Ok(None);
        }
        let id = target
            .ancestors()
            .skip(1)
            .find_map(|path| tree.id_for_path(path));
        let Some(id) = id else {
            self.follow = None;
            return Err("active file has no explorer root".into());
        };
        let load = tree.node(id).map(|node| node.load);
        tree.select(id);
        self.mark_view_changed();
        match load {
            Some(ExplorerLoadState::Loaded) => {
                self.follow = None;
                Err("active file is absent from the current explorer projection".into())
            }
            Some(ExplorerLoadState::Error) => {
                self.follow = None;
                Err("active file's explorer directory could not be loaded".into())
            }
            _ => Ok(self.load_request_for(view, id)),
        }
    }
    pub(in crate::app) fn reconcile_watch(&mut self, visible: bool) -> Result<(), String> {
        if self.watch_visible && !visible {
            self.epoch = Arc::new(());
            self.follow = None;
            self.next_loads.clear();
        }
        self.watch_visible = visible;
        if !visible {
            if self.watch_stream_id.take().is_some() {
                self.watch.cancel();
            }
            self.watch_observation = None;
            self.watch_reload.clear();
            return Ok(());
        }
        let projection = self.watch_projection_revision();
        let revision = self.tree().root_revision();
        if self
            .watch_observation
            .as_ref()
            .is_some_and(|(old_projection, old_revision, _)| {
                *old_projection == projection && *old_revision == revision
            })
        {
            return Ok(());
        }
        let directories = self.watched_directories();
        if self
            .watch_observation
            .as_ref()
            .is_some_and(|(_, old_revision, old_directories)| {
                *old_revision == revision && *old_directories == directories
            })
        {
            self.watch_observation = Some((projection, revision, directories));
            return Ok(());
        }
        match self
            .watch
            .reconcile(revision, self.roots.clone(), directories.clone())
        {
            Ok(stream_id) => {
                self.watch_observation = Some((projection, revision, directories));
                self.watch_stream_id = stream_id;
                Ok(())
            }
            Err(_) => {
                self.watch_stream_id = None;
                Err("explorer watch reconciliation failed".into())
            }
        }
    }
    pub(in crate::app) fn accept_watch(&mut self, batch: ExplorerWatchBatch, visible: bool) {
        if !visible
            || Some(batch.stream_id) != self.watch_stream_id
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
    pub(in crate::app) fn poll_watch(&mut self, visible: bool) -> Result<(), String> {
        for event in self.watch.poll() {
            match event {
                ExplorerWatchEvent::Batch(batch) => self.accept_watch(batch, visible),
                ExplorerWatchEvent::Failed {
                    stream_id,
                    root_revision,
                } if Some(stream_id) == self.watch_stream_id
                    && root_revision == self.tree().root_revision() =>
                {
                    self.watch_stream_id = None;
                    self.watch_observation = None;
                    return Err("explorer watch worker rejected the active set".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(in crate::app) fn next_reload(&mut self) -> Option<DirectoryLoad> {
        if self.pending() {
            return None;
        }
        while let Some(request) = self.next_loads.pop_front() {
            if self
                .tree_for(request.view)
                .and_then(|tree| tree.node(request.node))
                .is_some()
            {
                return Some(request);
            }
        }
        while let Some(path) = self.watch_reload.pop_front() {
            let Some(id) = self.tree().id_for_path(&path) else {
                continue;
            };
            if self.tree().node(id).is_some_and(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && self.any_view_watches_directory(id)
                    && node.load == ExplorerLoadState::Loaded
            }) {
                return self.load_request_for(self.active_view, id);
            }
        }
        // Retained searches keep reading even while an unfiltered clone has focus.
        self.view_ids().find_map(|view| {
            let tree = self.tree_for(view)?;
            if tree.filter().is_empty() {
                return None;
            }
            self.load_request_for(view, tree.unloaded_directory(SEARCH_ENTRY_LIMIT)?)
        })
    }
    pub(in crate::app) fn reset_watch_observation(&mut self) {
        self.watch_observation = None;
    }
    pub(in crate::app) fn projection_for(
        &mut self,
        view: ExplorerViewId,
        config: &crate::config::ExplorerConfig,
        body: crate::windows::WindowRect,
        git_status: &BTreeMap<PathBuf, crate::git::GitFileStatus>,
        title: String,
        root_label: &str,
    ) -> Result<ExplorerUiState, &'static str> {
        if self.tree_for(view).is_none() {
            return Err("explorer view is unavailable");
        }
        let active = self.active_view;
        self.active_view = view;
        let mut projection = self.projection(config, body, git_status, title, root_label);
        projection.searching &= view == active;
        self.active_view = active;
        Ok(projection)
    }
    pub(in crate::app) fn projection(
        &mut self,
        config: &crate::config::ExplorerConfig,
        body: crate::windows::WindowRect,
        git_status: &BTreeMap<PathBuf, crate::git::GitFileStatus>,
        title: String,
        root_label: &str,
    ) -> ExplorerUiState {
        self.tree_mut().ensure_selection();
        let tree = self.tree();
        let visible_ids = tree.visible_ids();
        let total = visible_ids.len();
        let selected_id = tree.selected_id();
        let selected = selected_id.and_then(|id| tree.visible_index(id));
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
                    text: {
                        let name = if node.kind == ExplorerKind::Root && self.root_count == 1 {
                            root_label.to_owned()
                        } else {
                            node.name.clone()
                        };
                        if matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                            && node.load == ExplorerLoadState::Loaded
                            && tree.is_expanded(node.id)
                            && node.children.is_empty()
                        {
                            format!("{name} (empty)")
                        } else {
                            name
                        }
                    },
                    role: name_role,
                });
                let right_segments = git_status
                    .map(|status| ExplorerUiSegment {
                        text: match status {
                            crate::git::GitFileStatus::Untracked => "?",
                            crate::git::GitFileStatus::Added => "A",
                            crate::git::GitFileStatus::Modified => "M",
                            crate::git::GitFileStatus::TypeChanged => "T",
                            crate::git::GitFileStatus::Deleted => "D",
                            crate::git::GitFileStatus::Renamed => "R",
                            crate::git::GitFileStatus::Conflicted => "!",
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
    pub(in crate::app) fn watch_stream_id(&self) -> Option<ExplorerWatchId> {
        self.watch_stream_id
    }
    #[cfg(test)]
    pub(in crate::app) fn watch_reconciles(&self) -> u64 {
        self.watch.reconcile_count()
    }
    #[cfg(test)]
    pub(in crate::app) fn watch_queue_empty(&self) -> bool {
        self.watch_reload.is_empty()
    }
    #[cfg(test)]
    pub(in crate::app) fn following(&self) -> bool {
        self.follow.is_some()
    }
}
pub(in crate::app) fn explorer_icon_role(
    color: crate::files::ExplorerIconColor,
) -> ExplorerSegmentRole {
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

pub(in crate::app) mod tree;

pub(in crate::app) mod commands;

#[cfg(test)]
mod tree_tests;

use crate::app::App;
use crate::app::search::MAXIMUM_PICKER_ITEMS;
use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::app::windows::NativeSurfaceTarget;
use crate::app::windows::rect_contains_point;
use crate::editor::RegisterShape;
use crate::editor::RegisterValue;
use crate::editor::text::TextBuffer;
use crate::files::ExplorerMutation;
use crate::ui::PromptKind;
use crate::windows::DockEdge;
use crate::windows::PANE_DEFAULT_RATIO;
use crate::windows::WindowRect;

impl App {
    pub(in crate::app) fn explorer_open_file(&mut self, split: bool) {
        if self
            .explorer
            .tree()
            .selected_node()
            .is_some_and(|node| matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory))
        {
            self.explorer_toggle();
            return;
        }
        if let Err(error) = self.documents.check_capacity() {
            self.show_error(error);
            return;
        }
        let request = self.explorer.prepare_open(
            split,
            (
                self.workspace.version(),
                self.configuration.binding_revision(),
            ),
            self.state.window_version(),
            self.workspace.roots(),
        );
        let (previous_request, request) = match request {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Some(id) = previous_request {
            self.documents.cancel_request(id);
        }
        match self.documents.open_explorer(request.path, request.roots) {
            Ok(id) => {
                self.set_explorer_focus(false);
                // Finish the requested focus change before capturing its async
                // navigation guard; a redraw must not look like new navigation.
                self.reconcile_editor_windows();
                let navigation = self
                    .window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view());
                if !self
                    .explorer
                    .track_open_request(request.ticket, id, navigation)
                {
                    self.documents.cancel_request(id);
                }
            }
            Err(error) => {
                self.explorer.fail_open(request.ticket);
                self.show_error(error);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_explorer_confirmation(&mut self) {
        if let Some(id) = self.explorer.cancel_confirmation() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_stale_explorer_intents(&mut self) {
        let ids = self.explorer.cancel_stale_intents(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        for id in ids {
            self.documents.cancel_request(id);
        }
        if self
            .input
            .prompt()
            .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
            && !self.explorer.rename_current(
                &self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            )
        {
            self.close_prompt();
        }
        if !self.explorer.confirmation_pending()
            && self
                .input
                .prompt()
                .is_some_and(|p| p.kind == PromptKind::ExplorerDelete)
        {
            self.close_prompt();
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_explorer_intents(&mut self) {
        for id in self.explorer.cancel_intents() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    fn queue_explorer_mutation(
        &mut self,
        operation: Result<ExplorerMutation, String>,
        confirmation: bool,
        rename_prompt: bool,
    ) -> Result<(), String> {
        let operation = operation?;
        if self.explorer.mutation_pending() {
            return Err("an explorer operation is already pending".into());
        }
        self.documents
            .check_capacity()
            .map_err(|error| error.to_string())?;
        let change = match &operation {
            ExplorerMutation::Rename { source, .. } | ExplorerMutation::Delete { path: source } => {
                Some(self.state.capture_path_change(source)?)
            }
            _ => None,
        };
        let context = crate::app::explorer::MutationContext {
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
            editor: self.state.window_version(),
            change,
        };
        let request = self.explorer.prepare_mutation(
            operation,
            context,
            self.workspace.roots(),
            SEARCH_MAXIMUM_FILE_BYTES as u64,
            confirmation,
        )?;
        let result = if rename_prompt {
            let source = self.explorer.submitted_rename_source();
            let target = request
                .operation
                .target()
                .expect("validated rename target")
                .into();
            self.documents.prepare_explorer_rename(source, target)
        } else {
            self.documents
                .prepare_explorer(request.operation, request.roots, request.maximum)
        }
        .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.track_mutation_request(request.ticket, result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            return Err(error);
        }
        self.notify_message("checking explorer operation…".into(), false);
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn create_explorer_path(&mut self, input: String) {
        if let Err(error) = self.queue_explorer_mutation(self.explorer.create(&input), false, false)
        {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn rename_explorer_path(&mut self, target: String) {
        if let Err(error) =
            self.queue_explorer_mutation(self.explorer.rename(&target), false, false)
        {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn open_explorer_rename_prompt(&mut self) {
        self.close_prompt();
        let request = match self.explorer.begin_rename(
            self.workspace.version(),
            self.configuration.binding_revision(),
            self.state.window_version(),
            self.workspace.roots(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let result = self.explorer.rename_inspection().and_then(|inspection| {
            let id = self
                .documents
                .inspect_explorer_rename(
                    request.path,
                    request.roots,
                    SEARCH_MAXIMUM_FILE_BYTES as u64,
                )
                .map_err(|error| error.to_string())?;
            inspection.register(id);
            Ok(())
        });
        if let Err(error) = result {
            self.explorer.cancel_rename();
            self.show_error(error);
            return;
        }
        self.open_prompt(PromptKind::ExplorerRename, request.initial);
        self.prompt_help(format!(
            "Relative to `{}`. Checking source… Enter renames; Esc cancels.",
            request.root.display()
        ));
    }
}

impl App {
    pub(in crate::app) fn submit_explorer_rename_prompt(&mut self, value: &str) {
        let operation = self.explorer.rename_value(
            value,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        if let Err(error) = self.queue_explorer_mutation(operation, false, true) {
            self.prompt_error(error);
        } else {
            self.prompt_help("Checking rename target… Esc cancels before application.".into());
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_explorer_rename_prompt(&mut self) {
        for id in self.explorer.cancel_rename() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    pub(in crate::app) fn request_explorer_delete(&mut self) {
        self.close_prompt();
        if let Err(error) = self.queue_explorer_mutation(self.explorer.delete(), true, false) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn confirm_explorer_delete(&mut self) {
        match self.explorer.confirm_delete(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) {
            Ok(prepared) => self.apply_prepared_explorer(prepared),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn apply_prepared_explorer(
        &mut self,
        prepared: crate::files::PreparedExplorerMutation,
    ) {
        let result = (|| {
            if let ExplorerMutation::Rename { source, target } = prepared.operation() {
                self.state.check_rename_destination(source, target)?;
            }
            self.documents
                .apply_explorer(prepared)
                .map_err(|e| e.to_string())
        })();
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.track_apply_request(result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn set_explorer_clipboard(&mut self, cut: bool) {
        match self.explorer.clipboard(cut, self.workspace.version()) {
            Ok(message) => self.notify_message(message, false),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn paste_explorer_clipboard(&mut self) {
        if let Err(error) = self.queue_explorer_mutation(
            self.explorer.paste(&self.workspace.version()),
            false,
            false,
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_explorer_io(&mut self, reply: crate::files::ExplorerIoReply) {
        use crate::app::explorer::MutationUpdate;
        if let crate::files::ExplorerIoReply::RenameInspected { id, result } = reply {
            match self.explorer.accept_rename_inspection(
                id,
                result,
                self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            ) {
                Some(Ok(root)) => self.prompt_help(format!(
                    "Relative to `{}`. Enter renames; Esc cancels.",
                    root.display()
                )),
                Some(Err(error)) => self.prompt_error(error),
                None => {}
            }
            return;
        }
        if let crate::files::ExplorerIoReply::Opened { id, path, result } = reply {
            let Some(reply) = self.explorer.accept_open(
                id,
                &path,
                (
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                ),
                &self.state.window_version(),
            ) else {
                return;
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            if let Err(error) = self
                .window_session
                .check_editor_source(&(&*self.state).into())
            {
                self.show_error(error);
                return;
            }
            let text = match result
                .map_err(|e| e.to_string())
                .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string()))
            {
                Ok(text) => text,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            let activate = self.window_session.navigation_current(
                &reply.navigation,
                &self.state.window_version(),
                self.state.active_view(),
            );
            let mut draft = self.state.draft();
            if activate {
                if reply.split
                    && self.state.active_view().is_some()
                    && let Err(error) = draft.split_document(PANE_DEFAULT_RATIO)
                {
                    self.show_error(error);
                    return;
                }
                if let Err(error) = draft.open_document(text, path) {
                    self.show_error(error);
                    return;
                }
            } else {
                if let Err(error) = draft.load_document(text, path) {
                    self.show_error(error);
                    return;
                }
            }
            if let Err(error) = self.commit_editor_draft(draft, None, activate) {
                self.show_error(error);
                return;
            }
            if activate {
                self.set_dashboard_visibility(false);
                self.ensure_cursor_visible();
            }
            return;
        }
        let Some(update) = self.explorer.accept_mutation(
            reply,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) else {
            return;
        };
        match update {
            MutationUpdate::Confirmation(path) => {
                self.open_confirmation(
                    PromptKind::ExplorerDelete,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .explorer_delete_title
                        .clone(),
                    explorer_delete_message(&path),
                    crate::ui::ConfirmationChoice::No,
                );
            }
            MutationUpdate::Apply(prepared) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.apply_prepared_explorer(prepared);
            }
            MutationUpdate::Failed(error) => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.prompt_error(error);
                } else {
                    self.show_error(error);
                }
            }
            MutationUpdate::Complete { operation, change } => {
                let reconciliation = (|| -> Result<(), String> {
                    self.window_session
                        .check_editor_source(&(&*self.state).into())?;
                    if let Some(change) = change {
                        let target = match &operation {
                            ExplorerMutation::Rename { target, .. } => Some(target.as_path()),
                            _ => None,
                        };
                        let (draft, recent) = self.state.reconcile_paths(change, target)?;
                        if let Some(draft) = draft {
                            self.commit_editor_draft(draft, None, false)?;
                        }
                        for path in recent {
                            self.record_recent_file(path);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = reconciliation {
                    self.show_error(error);
                } else {
                    self.notify_message(
                        format!(
                            "explorer operation complete: {}",
                            operation
                                .target()
                                .or_else(|| operation.source())
                                .map_or_else(
                                    || "path".into(),
                                    |path| self.workspace_relative_label(path)
                                )
                        ),
                        false,
                    );
                }
                self.refresh_explorer();
                self.start_git_status_refresh_silent();
            }
        }
    }
}

fn explorer_delete_message(path: &std::path::Path) -> String {
    format!("Permanently delete {path:?}?")
}

#[cfg(all(test, unix))]
#[test]
fn explorer_delete_message_uses_truthful_path_debug_escaping() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    assert_eq!(
        explorer_delete_message(std::path::Path::new("a  界\n\\b")),
        "Permanently delete \"a  界\\n\\\\b\"?"
    );
    let raw = PathBuf::from(OsString::from_vec(b"raw-\xff".to_vec()));
    assert_eq!(
        explorer_delete_message(&raw),
        "Permanently delete \"raw-\\xFF\"?"
    );
}
impl App {
    pub(in crate::app) fn move_explorer_selection(&mut self, delta: isize) {
        self.explorer.navigate(delta);
        self.sync_explorer_ui();
    }
}

impl App {
    pub(in crate::app) fn page_explorer_selection(&mut self, direction: isize) {
        let viewport = self
            .window_session
            .focused_explorer()
            .and_then(|view| self.window_session.native_ids().explorer_view(view))
            .and_then(|ids| self.window_session.layout()?.projection.window(ids.window))
            .map(|window| crate::ui::render::explorer::explorer_body_geometry(window.rect))
            .or(self.window_session.content().explorer)
            .map_or(0, |body| usize::from(body.height));
        self.move_explorer_selection(
            direction.saturating_mul(isize::try_from(viewport).unwrap_or(isize::MAX)),
        );
    }
}

impl App {
    pub(in crate::app) fn explorer_collapse_or_parent(&mut self) {
        self.explorer.parent();
        self.sync_explorer_ui();
    }
}

impl App {
    // EXPLORER_WATCH_FOREGROUND_BEGIN: typed event coordination only.

    pub(in crate::app) fn reconcile_explorer_watch(&mut self) {
        if self.ui.explorer.is_none() {
            self.cancel_explorer_intents();
        }
        if let Err(error) = self.explorer.reconcile_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn accept_explorer_watch_batch(
        &mut self,
        batch: crate::files::ExplorerWatchBatch,
    ) {
        self.explorer
            .accept_watch(batch, self.ui.explorer.is_some());
    }
}

impl App {
    pub(in crate::app) fn process_explorer_watch_events(&mut self) {
        if let Err(error) = self.explorer.poll_watch(self.ui.explorer.is_some()) {
            self.show_error(error);
        }
        self.start_next_explorer_watch_reload();
    }
}

impl App {
    pub(in crate::app) fn start_next_explorer_watch_reload(&mut self) {
        if let Some(request) = self.explorer.next_reload() {
            self.load_explorer_request(request);
        }
    }
}

impl App {
    // EXPLORER_WATCH_FOREGROUND_END

    pub(in crate::app) fn sync_explorer_ui(&mut self) {
        self.refresh_window_content();
        if self.ui.explorer.is_none() {
            return;
        }
        let title = self.message("explorer_title", &[]);
        let root_label = crate::ui::workspace_title(&self.state);
        let explorer_windows = self
            .window_session
            .native_ids()
            .explorer_views()
            .filter_map(|(view, ids)| {
                let rect = self
                    .window_session
                    .layout()?
                    .projection
                    .window(ids.window)?
                    .rect;
                Some((view, ids.window, rect))
            })
            .collect::<Vec<_>>();
        let focused = self.window_session.focused_explorer();
        let policy = &self.configuration.settings().ui.explorer;
        let decorations = self.git.decorations();
        let mut primary = None;
        let mut clones = Vec::new();
        for (view, window, rect) in explorer_windows {
            let body = if view == 1 {
                self.window_session
                    .content()
                    .explorer
                    .unwrap_or_else(|| crate::ui::render::explorer::explorer_body_geometry(rect))
            } else {
                crate::ui::render::explorer::explorer_body_geometry(rect)
            };
            let Ok(projection) = self.explorer.projection_for(
                crate::app::explorer::ExplorerViewId(view),
                policy,
                body,
                decorations,
                title.clone(),
                &root_label,
            ) else {
                continue;
            };
            if view == 1 {
                primary = Some(projection);
            } else {
                clones.push(crate::ui::ExplorerWindowUiState {
                    view,
                    window,
                    content: projection,
                    focused: focused == Some(view),
                });
            }
        }
        if primary.is_none() {
            let body = self.ui.surface_content.explorer.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
            primary = self
                .explorer
                .projection_for(
                    crate::app::explorer::PRIMARY_EXPLORER_VIEW,
                    policy,
                    body,
                    decorations,
                    title,
                    &root_label,
                )
                .ok();
        }
        self.ui.explorer = primary;
        self.ui.explorer_views = clones;
        self.reconcile_explorer_watch();
        self.reconcile_editor_windows();
    }
}

impl App {
    pub(in crate::app) fn open_explorer(&mut self) {
        self.window_session.allow_explorer();
        if self.explorer.tree().root_revision() != self.workspace.root_revision()
            || self.explorer.tree().visible_ids().is_empty()
        {
            self.explorer
                .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        }
        self.ui.explorer = Some(ExplorerUiState {
            title: String::new(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            matches: 0,
            query: String::new(),
            searching: false,
        });
        self.set_explorer_focus(true);
        self.sync_explorer_ui();
        self.git.retry_status();
        self.start_git_status_refresh_silent();
        let root_to_load = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root_to_load {
            self.load_explorer_node(root);
        }
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn load_explorer_node(&mut self, node: ExplorerNodeId) {
        match self.explorer.load(
            node,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    fn load_explorer_request(&mut self, request: crate::app::explorer::DirectoryLoad) {
        match self.explorer.load_request(
            request,
            self.workspace.version(),
            self.configuration.binding_revision(),
            MAXIMUM_PICKER_ITEMS,
        ) {
            Ok(()) => self.sync_explorer_ui(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn follow_active_file_in_explorer(&mut self) {
        let Some(path) = self
            .state
            .active_buffer_id()
            .and_then(|id| self.state.buffers[id].path.clone())
        else {
            self.show_error("active buffer has no file path");
            return;
        };
        if self.workspace.root_for_canonical_path(&path).is_none() {
            self.show_error("active file is outside workspace roots");
            return;
        }
        self.open_explorer();
        if self.ui.explorer.is_some() {
            self.explorer.follow(path);
            self.continue_explorer_follow();
        }
    }
}

impl App {
    pub(in crate::app) fn continue_explorer_follow(&mut self) {
        match self.explorer.continue_follow() {
            Ok(Some(request)) => self.load_explorer_request(request),
            Ok(None) => {}
            Err(error) => self.show_error(error),
        }
        self.sync_explorer_ui();
    }
}

impl App {
    /// Filter the tree by `query`; the folders not read yet are read in the background so
    /// the whole project is searched.
    pub(in crate::app) fn search_explorer(&mut self, query: String) {
        self.explorer.set_search(query);
        self.start_next_explorer_watch_reload();
        self.sync_explorer_ui();
    }
}

impl App {
    pub(in crate::app) fn explorer_toggle(&mut self) {
        let Some(node) = self.explorer.tree().selected_node().cloned() else {
            return;
        };
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => match node.load {
                ExplorerLoadState::Loaded => {
                    self.explorer.toggle_loaded(node.id);
                    self.sync_explorer_ui();
                }
                _ => self.load_explorer_node(node.id),
            },
            ExplorerKind::File | ExplorerKind::Symlink => self.explorer_open_file(false),
        }
    }
}

impl App {
    pub(in crate::app) fn process_explorer_result(&mut self) {
        let Some(result) = self.explorer.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        if let Err(error) = result {
            self.show_error(error);
        }
        self.explorer.select_search_match();
        self.sync_explorer_ui();
        self.continue_explorer_follow();
        self.start_next_explorer_watch_reload();
    }
}

impl App {
    pub(in crate::app) fn yank_explorer_path(&mut self) {
        let Some(node) = self.explorer.tree().selected_node() else {
            self.show_error("select an explorer path first");
            return;
        };
        let node_path = node.path.clone();
        let text = node_path.display().to_string();
        let policy = self.state.policy;
        self.state.registers.set(
            '"',
            RegisterValue {
                text: text.clone(),
                shape: RegisterShape::Character,
            },
            policy.registers_enabled,
            policy.mirror_named_to_unnamed,
        );
        self.state.mark_editor_changed();
        let label = self.workspace_relative_label(&node_path);
        self.notify_message(format!("yanked `{label}`"), false);
    }
}

impl App {
    pub(in crate::app) fn explorer_mouse_select(&mut self) {
        let Some(id) = self.explorer.take_clicked() else {
            return;
        };
        self.explorer.select(id);
        if self.window_session.focused_explorer().unwrap_or(1) == 1 {
            self.set_explorer_focus(true);
        }
        self.sync_explorer_ui();
        // As in neo-tree and VS Code, a click opens a file and toggles a folder.
        self.explorer_open_file(false);
    }
}

impl App {
    /// The tree row under the pointer in the layout now on screen.
    pub(in crate::app) fn explorer_row_at(
        &self,
        window: crate::windows::WindowId,
        column: u16,
        row: u16,
    ) -> Option<ExplorerNodeId> {
        let view = match self.window_session.native_ids().target_for_window(window)? {
            NativeSurfaceTarget::Explorer => 1,
            NativeSurfaceTarget::ExplorerView(view) => view,
            _ => return None,
        };
        let rect = self.compositor_projection()?.window(window)?.rect;
        let body = crate::ui::render::explorer::explorer_body_geometry(rect);
        if !rect_contains_point(body, column, row) {
            return None;
        }
        let explorer_ui = if view == 1 {
            self.ui.explorer.as_ref()
        } else {
            self.ui
                .explorer_views
                .iter()
                .find(|candidate| candidate.view == view)
                .map(|candidate| &candidate.content)
        };
        let index = usize::from(row.saturating_sub(body.y));
        explorer_ui?
            .rows
            .get(index)
            .map(|row| ExplorerNodeId(row.node_id))
    }
}

impl App {
    pub(in crate::app) fn refresh_explorer(&mut self) {
        self.cancel_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.sync_explorer_ui();
        let root = self.explorer.tree().selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root {
            self.load_explorer_node(root);
        }
    }
}

impl App {
    pub(in crate::app) fn explorer_edge(&self) -> DockEdge {
        match self.configuration.settings().ui.explorer.side {
            crate::config::PanelSide::Left => DockEdge::Left,
            crate::config::PanelSide::Right => DockEdge::Right,
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
