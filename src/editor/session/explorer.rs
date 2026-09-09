//! Explorer tree, directory lane, preview, watch and viewport ownership.
mod intent;
mod preview;
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
pub(in crate::editor::session) struct ExplorerSession {
    tree: ExplorerState,
    preview: Option<preview::Preview>,
    mutation: Option<intent::MutationIntent>,
    rename: Option<intent::RenamePrompt>,
    open: Option<intent::OpenIntent>,
    clipboard: Option<(PathBuf, bool, WorkspaceVersion)>,
    directory: DirectoryService,
    pending: Option<(ExplorerNodeId, u64, u64, WorkspaceVersion, u64, Arc<()>)>,
    epoch: Arc<()>,
    roots: Vec<PathBuf>,
    observed_roots: Vec<crate::workspace::RootIdentity>,
    root_count: usize,
    scroll: usize,
    hidden: bool,
    ignored: bool,
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
            tree: ExplorerState::default(),
            preview: None,
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
            scroll: 0,
            hidden: false,
            ignored: false,
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
        &self.tree
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
        self.tree.reset_roots(roots, revision);
        self.watch_observation = None;
        self.watch_reload.clear();
    }
    pub(in crate::editor::session) fn select(&mut self, id: ExplorerNodeId) {
        self.tree.select(id);
        self.retire_changed_preview_selection();
    }
    pub(in crate::editor::session) fn select_first_child(&mut self) {
        self.tree.select_first_child();
        self.retire_changed_preview_selection();
    }
    pub(in crate::editor::session) fn toggle_loaded(&mut self, id: ExplorerNodeId) {
        self.tree.toggle_loaded(id);
        self.retire_changed_preview_selection();
    }
    pub(in crate::editor::session) fn first_presented(&self) -> usize {
        usize::from(
            self.root_count == 1
                && self.tree.visible_ids().first().is_some_and(|id| {
                    self.tree
                        .node(*id)
                        .is_some_and(|node| node.kind == ExplorerKind::Root)
                }),
        )
    }
    pub(in crate::editor::session) fn navigate(&mut self, delta: isize) {
        self.tree.move_selection_from(delta, self.first_presented());
        self.retire_changed_preview_selection();
    }
    pub(in crate::editor::session) fn parent(&mut self) {
        self.tree.collapse_or_parent_from(self.first_presented());
        self.retire_changed_preview_selection();
    }
    pub(in crate::editor::session) fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
    }
    pub(in crate::editor::session) fn toggle_ignored(&mut self) {
        self.ignored = !self.ignored;
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
        let Some(path) = self.tree.node(node).map(|node| node.path.clone()) else {
            return Ok(());
        };
        let generation = self
            .directory
            .list(
                path,
                self.roots.clone(),
                self.tree.root_revision,
                self.hidden,
                self.ignored,
                maximum,
            )
            .map_err(|e| e.to_string())?;
        self.tree.begin_load(node, generation);
        self.pending = Some((
            node,
            generation,
            self.tree.root_revision,
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
        let (node, generation, revision, expected, policy, epoch) = self.pending.take()?;
        if expected != scope || policy != configuration || !Arc::ptr_eq(&epoch, &self.epoch) {
            // Only retire the old load in its original tree; IDs can be reused after reset.
            if revision == self.tree.root_revision {
                self.tree.set_error(node, generation);
            }
            return Some(Ok(()));
        }
        match result {
            Ok((listing, identities))
                if listing.generation == generation
                    && listing.root_revision == revision
                    && revision == self.tree.root_revision =>
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
                let root_selected = self.tree.selected_id() == Some(node)
                    && self
                        .tree
                        .node(node)
                        .is_some_and(|node| node.kind == ExplorerKind::Root);
                self.tree.apply_load(node, generation, revision, children);
                if root_selected && self.root_count == 1 {
                    self.tree.select_first_child();
                    self.retire_changed_preview_selection();
                }
                Some(Ok(()))
            }
            Ok(_) => {
                if revision == self.tree.root_revision {
                    self.tree.set_error(node, generation);
                }
                Some(Ok(()))
            }
            Err(error) => {
                self.tree.set_error(node, generation);
                Some(Err(error.to_string()))
            }
        }
    }
    pub(in crate::editor::session) fn follow(&mut self, path: PathBuf) {
        self.follow = Some(path);
    }
    pub(in crate::editor::session) fn continue_follow(
        &mut self,
    ) -> Result<Option<ExplorerNodeId>, String> {
        if self.pending() {
            return Ok(None);
        }
        let Some(target) = self.follow.as_ref() else {
            return Ok(None);
        };
        if let Some(id) = self.tree.id_for_path(target) {
            self.tree.select(id);
            self.retire_changed_preview_selection();
            self.follow = None;
            return Ok(None);
        }
        let id = target
            .ancestors()
            .skip(1)
            .find_map(|path| self.tree.id_for_path(path));
        let Some(id) = id else {
            self.follow = None;
            return Err("active file has no explorer root".into());
        };
        self.tree.select(id);
        self.retire_changed_preview_selection();
        if self
            .tree
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
        let observation = (self.tree.projection_generation(), self.tree.root_revision);
        if self.watch_observation == Some(observation) {
            return Ok(());
        }
        self.watch_observation = Some(observation);
        match self.watch.reconcile(
            observation.1,
            self.roots.clone(),
            self.tree.watched_directories(),
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
            || batch.root_revision != self.tree.root_revision
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
                    && root_revision == self.tree.root_revision =>
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
            let Some(id) = self.tree.id_for_path(&path) else {
                continue;
            };
            if self.tree.node(id).is_some_and(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && node.expanded
                    && node.load == ExplorerLoadState::Loaded
            }) {
                return Some(id);
            }
        }
        None
    }
    pub(in crate::editor::session) fn reconfigure_watch(
        &mut self,
        watch: Option<ExplorerWatchService>,
    ) {
        if let Some(watch) = watch {
            self.watch = watch;
            self.watch_generation = None;
            self.watch_reload.clear();
        }
        self.watch_observation = None;
    }
    pub(in crate::editor::session) fn projection(
        &mut self,
        config: &crate::config::ExplorerConfig,
        body: crate::ui::window::WindowRect,
        git_status: &BTreeMap<PathBuf, crate::workspace::git::GitFileStatus>,
        title: String,
    ) -> ExplorerUiState {
        let hidden_root_rows = self.first_presented();
        self.tree.ensure_selection_from(hidden_root_rows);
        let visible_ids = self.tree.visible_ids();
        let total = visible_ids.len().saturating_sub(hidden_root_rows);
        let selected_id = self.tree.selected_id();
        let selected = selected_id
            .and_then(|id| self.tree.visible_index(id))
            .and_then(|index| index.checked_sub(hidden_root_rows));
        let maximum = usize::from(body.height);
        let previous_scroll = self.scroll.min(total.saturating_sub(maximum));
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
            .skip(hidden_root_rows.saturating_add(scroll))
            .take(maximum)
            .filter_map(|id| self.tree.node(*id))
            .map(|node| {
                let lineage = self.tree.tree_lineage(node.id);
                let mut tree = String::new();
                for (index, is_last) in lineage.iter().enumerate() {
                    let final_segment = index + 1 == lineage.len();
                    tree.push_str(if final_segment {
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
                        ExplorerKind::Root | ExplorerKind::Directory => {
                            ExplorerSegmentRole::Directory
                        }
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
                let (icon, icon_role) = match (node.kind, node.load, node.expanded) {
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
                        (glyphs.directory_open.clone(), ExplorerSegmentRole::Icon)
                    }
                    (ExplorerKind::Root | ExplorerKind::Directory, _, false) => {
                        (glyphs.directory.clone(), ExplorerSegmentRole::Icon)
                    }
                    (ExplorerKind::File, _, _) => (glyphs.file.clone(), ExplorerSegmentRole::Icon),
                    (ExplorerKind::Symlink, _, _) => {
                        (glyphs.symlink.clone(), ExplorerSegmentRole::Icon)
                    }
                };
                let mut segments = Vec::with_capacity(4);
                if !tree.is_empty() {
                    segments.push(ExplorerUiSegment {
                        text: tree,
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
                    text: node.name.clone(),
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
        self.scroll = scroll;
        ExplorerUiState {
            title,
            total,
            rows,
            scroll,
            loading: self.pending.is_some(),
            preview_lines: self.preview_lines(),
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
fn explorer_icon_role(color: crate::files::ExplorerIconColor) -> ExplorerSegmentRole {
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

pub(in crate::editor::session) fn explorer_watch_policy(
    config: &crate::config::ExplorerWatchConfig,
) -> crate::files::ExplorerWatchPolicy {
    crate::files::ExplorerWatchPolicy {
        enabled: config.enabled,
        poll_interval: std::time::Duration::from_millis(config.poll_interval_ms),
        debounce: std::time::Duration::from_millis(config.debounce_ms),
        maximum_directories: config.maximum_directories,
        maximum_entries_per_directory: config.maximum_entries_per_directory,
        maximum_events: config.maximum_events,
        maximum_path_bytes: config.maximum_path_bytes,
        queue_capacity: config.queue_capacity,
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
        let config = crate::config::load_bundled_config();
        let mut owner = ExplorerSession::new(
            ExplorerWatchService::start(explorer_watch_policy(&config.config.ui.explorer.watch))
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
        let node = owner.tree.selected_id().unwrap();
        owner.reconcile_watch(true).unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        owner.reconcile_watch(false).unwrap();
        drain(&mut owner, source.clone());
        assert!(owner.tree.id_for_path(&path).is_none());
        assert!(owner.next_reload().is_none());
        owner.reconcile_watch(true).unwrap();
        owner.load(node, source.clone(), 1, 100).unwrap();
        drain(&mut owner, source);
        assert!(owner.tree.id_for_path(&path).is_some());
    }
    #[test]
    fn retired_directory_policy_and_preview_selection_have_terminal_dispositions() {
        use crate::files::RequestId;
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let (mut owner, source) = fixture(root.path());
        let node = owner.tree.selected_id().unwrap();
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
            owner.tree.node(node).unwrap().load,
            ExplorerLoadState::Error
        );
        owner.load(node, source.clone(), 2, 100).unwrap();
        wait(&mut owner);
        assert_eq!(
            owner.tree.node(node).unwrap().load,
            ExplorerLoadState::Loaded
        );
        let a = owner.tree.id_for_path(&a).unwrap();
        let b = owner.tree.id_for_path(&b).unwrap();
        let policy = crate::config::ExplorerConfig {
            preview_enabled: true,
            maximum_preview_bytes: 16,
            maximum_preview_lines: 2,
            ..Default::default()
        };
        owner.select(a);
        let request = owner
            .prepare_preview(source.clone(), 2, &policy, "binary", "failed: {diagnostic}")
            .1
            .unwrap();
        assert!(owner.admit_preview(request.ticket, Ok(RequestId(1))));
        owner.select(b);
        owner.select(a); // Selecting the same path again cannot revive the retired ticket.
        assert!(owner.accept_preview(RequestId(1), &Ok(vec![255; 32]), source.clone(), 2));
        assert!(owner.preview_lines().is_empty());
        let request = owner
            .prepare_preview(source.clone(), 2, &policy, "binary", "failed: {diagnostic}")
            .1
            .unwrap();
        assert!(owner.admit_preview(request.ticket, Ok(RequestId(2))));
        assert!(owner.accept_preview(
            RequestId(2),
            &Ok(b"one\0\ntwo\nthree".to_vec()),
            source.clone(),
            2
        ));
        assert_eq!(owner.preview_lines(), ["one", "two"]);
        assert!(!owner.accept_preview(RequestId(2), &Ok(b"duplicate".to_vec()), source.clone(), 2));
        owner.clear_preview();
        let request = owner
            .prepare_preview(source.clone(), 2, &policy, "binary", "failed: {diagnostic}")
            .1
            .unwrap();
        assert!(owner.admit_preview(request.ticket, Ok(RequestId(3))));
        assert!(owner.accept_preview(RequestId(3), &Ok(vec![255; 32]), source.clone(), 2));
        assert_eq!(
            owner.preview_lines(),
            ["failed: preview exceeds admitted byte limit"]
        );
        owner.reconcile_watch(true).unwrap();
        owner.reconcile_watch(false).unwrap();
        assert!(owner.preview_lines().is_empty());
    }
    #[test]
    #[ignore = "explicit explorer ownership cost probe"]
    fn explorer_owner_cost_probe() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..40 {
            std::fs::write(root.path().join(format!("file-{n:02}.txt")), "file").unwrap();
        }
        let (mut owner, source) = fixture(root.path());
        let node = owner.tree.selected_id().unwrap();
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
