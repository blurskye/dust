use super::*;

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

impl InteractiveApplication {
    pub(super) fn explorer_first_presented_index(&self) -> usize {
        usize::from(
            self.workspace.roots().len() == 1
                && self.explorer.visible_ids().first().is_some_and(|id| {
                    self.explorer
                        .node(*id)
                        .is_some_and(|node| node.kind == ExplorerKind::Root)
                }),
        )
    }

    pub(super) fn move_explorer_selection(&mut self, delta: isize) {
        let first_presented = self.explorer_first_presented_index();
        self.explorer.move_selection_from(delta, first_presented);
        self.sync_explorer_ui();
    }

    pub(super) fn page_explorer_selection(&mut self, direction: isize) {
        let viewport = usize::from(self.workspace_render_height().saturating_sub(4))
            .max(1)
            .min(self.configuration.settings.ui.explorer.maximum_visible_rows);
        self.move_explorer_selection(
            direction.saturating_mul(isize::try_from(viewport).unwrap_or(isize::MAX)),
        );
    }

    pub(super) fn explorer_collapse_or_parent(&mut self) {
        let first_presented = self.explorer_first_presented_index();
        self.explorer.collapse_or_parent_from(first_presented);
        self.sync_explorer_ui();
    }

    // EXPLORER_WATCH_FOREGROUND_BEGIN: typed event coordination only.

    pub(super) fn reconcile_explorer_watch(&mut self) {
        if self.ui.explorer.is_none() {
            if self.explorer_watch_generation.take().is_some() {
                self.explorer_watch.cancel();
            }
            self.explorer_watch_observation = None;
            self.explorer_watch_reload_queue.clear();
            return;
        }
        let observation = (
            self.explorer.projection_generation(),
            self.workspace.root_revision(),
        );
        if self.explorer_watch_observation == Some(observation) {
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.explorer_watch.reconcile(
            observation.1,
            roots,
            self.explorer.watched_directories(),
        ) {
            Ok(generation) => {
                self.explorer_watch_generation = generation;
                self.explorer_watch_observation = Some(observation)
            }
            Err(_) => {
                self.explorer_watch_generation = None;
                self.explorer_watch_observation = Some(observation);
                self.show_error("explorer watch reconciliation failed")
            }
        }
    }

    pub(super) fn accept_explorer_watch_batch(&mut self, batch: ExplorerWatchBatch) {
        if Some(batch.generation) != self.explorer_watch_generation
            || batch.root_revision != self.workspace.root_revision()
            || self.ui.explorer.is_none()
        {
            return;
        }
        for directory in batch.directories {
            if !self.explorer_watch_reload_queue.contains(&directory) {
                self.explorer_watch_reload_queue.push_back(directory)
            }
        }
    }

    pub(super) fn process_explorer_watch_events(&mut self) {
        for event in self.explorer_watch.poll() {
            match event {
                ExplorerWatchEvent::Batch(batch) => self.accept_explorer_watch_batch(batch),
                ExplorerWatchEvent::Failed {
                    generation,
                    root_revision,
                } if Some(generation) == self.explorer_watch_generation
                    && root_revision == self.workspace.root_revision() =>
                {
                    self.explorer_watch_generation = None;
                    self.show_error("explorer watch worker rejected the active set")
                }
                _ => {}
            }
        }
        self.start_next_explorer_watch_reload();
    }

    pub(super) fn start_next_explorer_watch_reload(&mut self) {
        if self.explorer_pending.is_some() {
            return;
        }
        while let Some(path) = self.explorer_watch_reload_queue.pop_front() {
            let Some(node) = self.explorer.id_for_path(&path) else {
                continue;
            };
            if self.explorer.node(node).is_some_and(|node| {
                matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory)
                    && node.expanded
                    && matches!(node.load, ExplorerLoadState::Loaded)
            }) {
                self.load_explorer_node(node);
                break;
            }
        }
    }

    // EXPLORER_WATCH_FOREGROUND_END

    pub(super) fn sync_explorer_ui(&mut self) {
        if self.ui.explorer.is_none() {
            return;
        }
        let hidden_root_rows = self.explorer_first_presented_index();
        self.explorer.ensure_selection_from(hidden_root_rows);
        let visible_ids = self.explorer.visible_ids();
        let total = visible_ids.len().saturating_sub(hidden_root_rows);
        let selected_id = self.explorer.selected_id();
        let selected = selected_id
            .and_then(|id| self.explorer.visible_index(id))
            .and_then(|index| index.checked_sub(hidden_root_rows));
        let viewport_rows = usize::from(self.workspace_render_height().saturating_sub(4)).max(1);
        let maximum = self
            .configuration
            .settings
            .ui
            .explorer
            .maximum_visible_rows
            .min(viewport_rows);
        let previous_scroll = self
            .ui
            .explorer
            .as_ref()
            .map_or(0, |explorer| explorer.scroll)
            .min(total.saturating_sub(maximum));
        let scroll = selected.map_or(previous_scroll, |selected| {
            if selected < previous_scroll {
                selected
            } else if selected >= previous_scroll.saturating_add(maximum) {
                selected.saturating_sub(maximum.saturating_sub(1))
            } else {
                previous_scroll
            }
        });
        let rich_icons = self.configuration.settings.ui.explorer.icon_mode
            == crate::config::ExplorerIconMode::NerdFont;
        let glyphs = self
            .configuration
            .settings
            .ui
            .explorer
            .active_glyphs()
            .clone();
        let rows = visible_ids
            .iter()
            .skip(hidden_root_rows.saturating_add(scroll))
            .take(maximum)
            .filter_map(|id| self.explorer.node(*id))
            .map(|node| {
                let lineage = self.explorer.tree_lineage(node.id);
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
                let git_status = self.git_status_decorations.get(&node.path);
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
        self.ui.explorer = Some(ExplorerUiState {
            title: self.message("explorer_title", &[]),
            total,
            rows,
            scroll,
            loading: self.explorer_pending.is_some(),
            preview_lines: self.explorer_preview_lines.clone(),
        });
        self.request_explorer_preview();
        self.reconcile_explorer_watch();
        self.reconcile_editor_windows();
    }

    pub(super) fn request_explorer_preview(&mut self) {
        if !self.configuration.settings.ui.explorer.preview_enabled {
            return;
        }
        let Some(node) = self.explorer.selected_node() else {
            return;
        };
        if !matches!(node.kind, ExplorerKind::File) {
            self.explorer_preview_path = None;
            self.explorer_preview_lines.clear();
            return;
        }
        let path = node.path.clone();
        if self.explorer_preview_path.as_ref() == Some(&path) {
            return;
        }
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        let generation = self.explorer_preview_generation;
        self.explorer_preview_path = Some(path.clone());
        self.explorer_preview_lines.clear();
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        if let Ok(id) = self.vfs.read_scoped_bounded(
            path.clone(),
            roots,
            self.configuration
                .settings
                .ui
                .explorer
                .maximum_preview_bytes as u64,
        ) {
            self.pending_vfs
                .insert(id, PendingVfs::ExplorerPreview { path, generation });
        }
    }

    pub(super) fn open_explorer(&mut self) {
        if !self.configuration.settings.ui.explorer.bundled_enabled {
            self.show_error("the bundled explorer contribution is disabled");
            return;
        }
        self.surface_windows.allow_explorer();
        if self.explorer.root_revision != self.workspace.root_revision()
            || self.explorer.visible_ids().is_empty()
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
            preview_lines: Vec::new(),
        });
        self.ui.explorer_focused = true;
        self.sync_explorer_ui();
        self.git_status_silent_attempted = None;
        self.start_git_status_refresh_silent();
        let root_to_load = self.explorer.selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root_to_load {
            self.load_explorer_node(root);
        }
        self.ensure_cursor_visible();
    }

    pub(super) fn load_explorer_node(&mut self, node: ExplorerNodeId) {
        if self.explorer_pending.is_some() {
            return;
        }
        let Some(path) = self.explorer.node(node).map(|node| node.path.clone()) else {
            return;
        };
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.directory_service.list(
            path,
            roots,
            self.workspace.root_revision(),
            self.explorer_hidden,
            self.explorer_ignored,
            self.configuration.settings.workspace.maximum_picker_items,
        ) {
            Ok(generation) => {
                self.explorer.begin_load(node, generation);
                self.explorer_pending = Some((node, generation, self.workspace.root_revision()));
                self.sync_explorer_ui();
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn follow_active_file_in_explorer(&mut self) {
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
        self.explorer_follow_path = Some(path);
        self.open_explorer();
        self.continue_explorer_follow();
    }

    pub(super) fn continue_explorer_follow(&mut self) {
        if self.explorer_pending.is_some() {
            return;
        }
        let Some(target) = self.explorer_follow_path.clone() else {
            return;
        };
        if let Some(id) = self.explorer.id_for_path(&target) {
            self.explorer.select(id);
            self.explorer_follow_path = None;
            self.sync_explorer_ui();
            return;
        }
        let mut ancestor = target.parent();
        let mut found = None;
        while let Some(path) = ancestor {
            if let Some(id) = self.explorer.id_for_path(path) {
                found = Some(id);
                break;
            }
            ancestor = path.parent()
        }
        let Some(id) = found else {
            self.explorer_follow_path = None;
            self.show_error("active file has no explorer root");
            return;
        };
        self.explorer.select(id);
        let Some(node) = self.explorer.node(id).cloned() else {
            return;
        };
        if matches!(node.load, ExplorerLoadState::Loaded) {
            self.explorer_follow_path = None;
            self.show_error("active file is absent from the current explorer projection");
            self.sync_explorer_ui()
        } else {
            self.load_explorer_node(id)
        }
    }

    pub(super) fn explorer_toggle(&mut self) {
        let Some(node) = self.explorer.selected_node().cloned() else {
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

    pub(super) fn process_explorer_result(&mut self) {
        let Some(result) = self.directory_service.try_result() else {
            return;
        };
        let Some((node, generation, revision)) = self.explorer_pending.take() else {
            return;
        };
        match result {
            Ok(listing)
                if listing.generation == generation
                    && listing.root_revision == revision
                    && revision == self.workspace.root_revision() =>
            {
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
                let selected_loaded_root = self.explorer.selected_id() == Some(node)
                    && self
                        .explorer
                        .node(node)
                        .is_some_and(|node| node.kind == ExplorerKind::Root);
                self.explorer
                    .apply_load(node, generation, revision, children);
                if selected_loaded_root && self.workspace.roots().len() == 1 {
                    self.explorer.select_first_child();
                }
            }
            Ok(_) => {}
            Err(error) => {
                self.explorer.set_error(node, generation);
                self.show_error(error);
            }
        }
        self.sync_explorer_ui();
        self.continue_explorer_follow();
        self.start_next_explorer_watch_reload();
    }

    pub(super) fn explorer_open_file(&mut self, split: bool) {
        let Some(node) = self.explorer.selected_node() else {
            return;
        };
        if matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory) {
            self.explorer_toggle();
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.read_scoped(node.path.clone(), roots) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::ExplorerOpen { split });
                self.ui.explorer_focused = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    fn explorer_selected_parent(&self) -> Option<PathBuf> {
        let node = self.explorer.selected_node()?;
        match node.kind {
            ExplorerKind::Root | ExplorerKind::Directory => Some(node.path.clone()),
            ExplorerKind::File | ExplorerKind::Symlink => node.path.parent().map(Path::to_path_buf),
        }
    }

    fn explorer_selected_mutable_path(&mut self) -> Option<PathBuf> {
        let Some(node) = self.explorer.selected_node() else {
            self.show_error("select an explorer path first");
            return None;
        };
        if node.kind == ExplorerKind::Root {
            self.show_error("workspace roots cannot be renamed or deleted from the explorer");
            return None;
        }
        Some(node.path.clone())
    }

    fn queue_explorer_mutation(&mut self, operation: ExplorerMutation) {
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.vfs.explorer_mutate(
            operation,
            roots,
            self.configuration
                .settings
                .workspace
                .search
                .maximum_file_bytes,
        ) {
            Ok(id) => {
                self.pending_vfs.insert(id, PendingVfs::ExplorerMutation);
                self.ui.notification = Some("applying explorer operation…".into());
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn create_explorer_path(&mut self, input: String) {
        let directory = input.ends_with('/') || input.ends_with(std::path::MAIN_SEPARATOR);
        let input = input.trim_end_matches(['/', std::path::MAIN_SEPARATOR]);
        if input.is_empty() {
            self.show_error("explorer path must not be empty");
            return;
        }
        let Some(parent) = self.explorer_selected_parent() else {
            self.show_error("select an explorer directory first");
            return;
        };
        let entered = PathBuf::from(input);
        let path = if entered.is_absolute() {
            entered
        } else {
            parent.join(entered)
        };
        self.queue_explorer_mutation(ExplorerMutation::Create { path, directory });
    }

    pub(super) fn rename_explorer_path(&mut self, target: String) {
        if target.trim().is_empty() {
            self.show_error("rename target must not be empty");
            return;
        }
        let Some(source) = self.explorer_selected_mutable_path() else {
            return;
        };
        let entered = PathBuf::from(target);
        let target = if entered.is_absolute() {
            entered
        } else {
            source
                .parent()
                .map_or_else(|| entered.clone(), |parent| parent.join(&entered))
        };
        self.queue_explorer_mutation(ExplorerMutation::Rename { source, target });
    }

    pub(super) fn request_explorer_delete(&mut self) {
        let Some(path) = self.explorer_selected_mutable_path() else {
            return;
        };
        self.explorer_delete_target = Some(path.clone());
        self.open_prompt(PromptKind::ExplorerDelete, String::new());
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.help = Some(format!(
                "Permanently deletes `{}`. Type `delete`.",
                path.display()
            ));
        }
    }

    pub(super) fn confirm_explorer_delete(&mut self, phrase: String) {
        let Some(path) = self.explorer_delete_target.take() else {
            self.show_error("explorer delete request is stale");
            return;
        };
        if phrase.trim() != "delete" {
            self.show_error("explorer delete confirmation did not match");
            return;
        }
        self.queue_explorer_mutation(ExplorerMutation::Delete { path });
    }

    pub(super) fn set_explorer_clipboard(&mut self, cut: bool) {
        let Some(path) = self.explorer_selected_mutable_path() else {
            return;
        };
        self.explorer_clipboard = Some((path.clone(), cut));
        self.ui.notification = Some(format!(
            "{} `{}`; select a destination and press p",
            if cut { "moving" } else { "copied" },
            path.display()
        ));
        self.ui.notification_error = false;
    }

    pub(super) fn paste_explorer_clipboard(&mut self) {
        let Some((source, cut)) = self.explorer_clipboard.clone() else {
            self.show_error("explorer clipboard is empty");
            return;
        };
        let Some(parent) = self.explorer_selected_parent() else {
            self.show_error("select an explorer destination first");
            return;
        };
        let Some(name) = source.file_name() else {
            self.show_error("copied explorer path has no file name");
            return;
        };
        let target = parent.join(name);
        self.queue_explorer_mutation(if cut {
            ExplorerMutation::Rename { source, target }
        } else {
            ExplorerMutation::Copy { source, target }
        });
    }

    pub(super) fn yank_explorer_path(&mut self) {
        let Some(node) = self.explorer.selected_node() else {
            self.show_error("select an explorer path first");
            return;
        };
        let text = node.path.display().to_string();
        self.state.registers.set(
            '"',
            RegisterValue {
                text: text.clone(),
                shape: RegisterShape::Character,
            },
            self.state.policy.registers_enabled,
            self.state.policy.mirror_named_to_unnamed,
        );
        self.ui.notification = Some(format!("yanked `{text}`"));
        self.ui.notification_error = false;
    }

    pub(super) fn explorer_mouse_select(&mut self) {
        let Some((column, row)) = self.mouse_position else {
            return;
        };
        let Some(rect) = self.explorer_window_rect() else {
            return;
        };
        if !rect_contains_point(rect, column, row) || row < rect.y.saturating_add(3) {
            return;
        }
        let Some(explorer_ui) = self.ui.explorer.as_ref() else {
            return;
        };
        let index = usize::from(row.saturating_sub(rect.y).saturating_sub(3));
        let Some(id) = explorer_ui.rows.get(index).map(|row| row.node_id) else {
            return;
        };
        self.explorer.select(ExplorerNodeId(id));
        self.ui.explorer_focused = true;
        self.sync_explorer_ui();
    }

    pub(super) fn refresh_explorer(&mut self) {
        self.explorer_pending = None;
        self.explorer_watch_reload_queue.clear();
        self.explorer_watch_observation = None;
        self.explorer_preview_path = None;
        self.explorer_preview_lines.clear();
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.sync_explorer_ui();
        let root = self.explorer.selected_node().and_then(|node| {
            (node.kind == ExplorerKind::Root && node.load != ExplorerLoadState::Loaded)
                .then_some(node.id)
        });
        if let Some(root) = root {
            self.load_explorer_node(root);
        }
    }
}
