use super::*;

impl InteractiveApplication {
    pub(super) fn effective_large_file(&self, buffer: crate::editor::BufferId) -> bool {
        match self
            .large_file_overrides
            .get(&buffer)
            .copied()
            .unwrap_or(LargeFileOverride::Auto)
        {
            LargeFileOverride::On => true,
            LargeFileOverride::Off => false,
            LargeFileOverride::Auto => self.state.buffers.get(buffer).is_some_and(|buffer| {
                self.configuration.settings.editor.large_file.enabled
                    && (buffer.text.len_bytes()
                        >= self.configuration.settings.editor.large_file.byte_threshold
                        || buffer.text.len_lines()
                            >= self.configuration.settings.editor.large_file.line_threshold)
            }),
        }
    }

    pub(super) fn process_large_file_modes(&mut self) {
        for (buffer, state) in &self.state.buffers {
            if !self.large_file_overrides.contains_key(&buffer)
                && let Some(mode) = state
                    .path
                    .as_ref()
                    .and_then(|path| self.persisted_large_file_overrides.get(path))
                    .copied()
            {
                self.large_file_overrides.insert(buffer, mode);
            }
        }
        let live = self.state.buffers.keys().collect::<Vec<_>>();
        self.large_file_overrides
            .retain(|buffer, _| live.contains(buffer));
        self.ui.large_file_buffers = live
            .into_iter()
            .filter(|buffer| self.effective_large_file(*buffer))
            .collect();
    }

    pub(super) fn persistence_policy(&self) -> PersistencePolicy {
        PersistencePolicy {
            maximum_file_bytes: self.configuration.settings.persistence.maximum_file_bytes,
            maximum_entries: self.configuration.settings.persistence.maximum_entries,
        }
    }

    pub(super) fn load_persistence(&mut self) {
        let configured = &self.configuration.settings.persistence;
        if !configured.enabled || !configured.restore_on_startup || !self.persistence_path.exists()
        {
            return;
        }
        if let Ok(id) = self
            .persistence
            .read(self.persistence_path.clone(), self.persistence_policy())
        {
            self.persistence_pending = Some(id);
        }
    }

    pub(super) fn capture_window_rules(&self) -> Vec<PersistedWindowRule> {
        let mut rules = self
            .surface_windows
            .semantic_defaults(self.active_terminal.map(|id| id.0));
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.explorer")
            && let Some(ids) = self.surface_windows.explorer()
            && let Some(window) = self.windows.window(ids.window)
        {
            rules.push(("builtin.explorer", window.placement));
        }
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.terminal")
            && let Some(ids) = self
                .active_terminal
                .and_then(|id| self.surface_windows.terminal(id.0))
            && let Some(window) = self.windows.window(ids.window)
        {
            rules.push(("builtin.terminal", window.placement));
        }
        rules
            .into_iter()
            .filter_map(|(content_type, placement)| {
                persisted_window_placement(placement).map(|placement| PersistedWindowRule {
                    content_type: content_type.into(),
                    placement,
                })
            })
            .collect()
    }

    pub(super) fn prepare_configured_window_rules(
        &self,
        windows: &crate::config::WindowsConfig,
    ) -> Result<(WindowGraph, NativeSurfaceBridge), String> {
        let placements = configured_window_placements(windows)?;
        let mut graph = self.windows.clone();
        let mut surfaces = self.surface_windows.clone();
        surfaces
            .set_configured_placements(placements)
            .map_err(|error| error.to_string())?;
        let signature = self.native_surface_signature(&self.editor_windows);
        let mut terminals = self
            .terminals
            .iter()
            .map(|(id, terminal)| (id.0, terminal.profile.clone()))
            .collect::<Vec<_>>();
        terminals.sort_unstable_by_key(|(id, _)| *id);
        surfaces
            .reconcile(
                &mut graph,
                signature,
                &self.message("explorer_title", &[]),
                &self.configuration.dashboard.identity,
                self.ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                &terminals,
            )
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        graph
            .revision()
            .ok_or_else(|| "window graph revision is exhausted".to_owned())?;
        Ok((graph, surfaces))
    }

    pub(super) fn apply_persisted_window_rules(
        &mut self,
        rules: &[PersistedWindowRule],
    ) -> Result<(), String> {
        validate_window_rules(rules).map_err(|error| error.to_string())?;
        let mut graph = self.windows.clone();
        let mut candidate = self.surface_windows.clone();
        for rule in rules {
            let placement = restored_window_placement(&rule.placement)?;
            candidate
                .set_semantic_default(&rule.content_type, placement)
                .map_err(|error| error.to_string())?;
        }
        let signature = self.native_surface_signature(&self.editor_windows);
        let mut terminals = self
            .terminals
            .iter()
            .map(|(id, terminal)| (id.0, terminal.profile.clone()))
            .collect::<Vec<_>>();
        terminals.sort_unstable_by_key(|(id, _)| *id);
        candidate
            .reconcile(
                &mut graph,
                signature,
                &self.message("explorer_title", &[]),
                &self.configuration.dashboard.identity,
                self.ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                &terminals,
            )
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        graph
            .revision()
            .ok_or_else(|| "window graph revision is exhausted".to_owned())?;

        let requires_projection = graph.active_tab().is_some();
        self.windows = graph;
        self.surface_windows = candidate;
        // The dry-run candidate is accepted, but the application projection
        // must be rebuilt against its newly published graph revision.
        self.surface_windows.invalidate();
        self.reconcile_editor_windows();
        if requires_projection && self.compositor_projection().is_none() {
            return Err("persisted window projection could not be published".into());
        }
        Ok(())
    }

    pub(super) fn capture_session(&self) -> Option<PersistedSession> {
        let mut tabs = Vec::new();
        let mut active_tab = 0_usize;
        for (index, (tab_id, tab)) in self.state.tabs.iter().enumerate() {
            let Some(layout) = capture_persisted_layout(&self.state, &tab.root) else {
                continue;
            };
            if Some(tab_id) == self.state.active_tab {
                active_tab = tabs.len();
            }
            let focused_path = self
                .state
                .panes
                .get(tab.focused)
                .and_then(|pane| self.state.views.get(pane.view))
                .and_then(|view| self.state.buffers.get(view.buffer))
                .and_then(|buffer| buffer.path.clone());
            tabs.push(PersistedTab {
                layout,
                focused_path,
            });
            let _ = index;
        }
        (!tabs.is_empty()).then(|| PersistedSession {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            tabs,
            active_tab,
            window_rules: self.capture_window_rules(),
        })
    }

    pub(super) fn save_persistence(&mut self) {
        let configured = &self.configuration.settings.persistence;
        if !configured.enabled || !configured.save_on_exit || self.persistence_pending.is_some() {
            return;
        }
        let state = PersistedState {
            version: STATE_VERSION,
            session: self.capture_session(),
            recent_files: self.recent_files.clone(),
            marks: self.marks.clone(),
            jumps: self.jump_list.clone(),
            changes: self.change_list.clone(),
            command_history: if configured.persist_command_history {
                self.command_history.clone()
            } else {
                Vec::new()
            },
            search_history: if configured.persist_search_history {
                self.search_history.clone()
            } else {
                Vec::new()
            },
            breakpoints: self
                .dap_breakpoints
                .values()
                .flatten()
                .take(self.configuration.settings.debug.maximum_breakpoints)
                .map(|row| PersistedBreakpoint {
                    path: row.path.clone(),
                    line: row.line,
                    column: row.column,
                    condition: row.condition.clone(),
                    hit_condition: row.hit_condition.clone(),
                    log_message: row.log_message.clone(),
                })
                .collect(),
            large_file_overrides: self
                .large_file_overrides
                .iter()
                .filter_map(|(id, mode)| {
                    let path = self.state.buffers.get(*id)?.path.clone()?;
                    Some(PersistedLargeFileOverride {
                        path,
                        mode: mode_name_large(*mode).into(),
                    })
                })
                .take(configured.maximum_entries)
                .collect(),
            watches: self
                .dap_watches
                .iter()
                .take(self.configuration.settings.debug.maximum_watches)
                .map(|expression| PersistedWatch {
                    expression: expression.clone(),
                })
                .collect(),
            metadata: self
                .lsp_nix_workspace_preferred
                .iter()
                .map(|id| (format!("lsp.runtime.{id}"), "nix".into()))
                .chain(
                    self.lsp_managed_preferred
                        .iter()
                        .map(|id| (format!("lsp.runtime.{id}"), "managed".into())),
                )
                .chain(
                    self.lsp_managed_versions
                        .iter()
                        .map(|(id, version)| (format!("lsp.version.{id}"), version.clone())),
                )
                .collect(),
        };
        if let Ok(id) = self.persistence.write(
            self.persistence_path.clone(),
            state,
            self.persistence_policy(),
        ) {
            self.persistence_pending = Some(id);
        }
    }

    pub(super) fn process_persistence_events(&mut self) {
        while let Ok(Some(event)) = self.persistence.try_event() {
            match event {
                PersistenceEvent::Loaded { generation, state }
                    if self.persistence_pending == Some(generation) =>
                {
                    let session = state.session.clone();
                    let valid_lsp = |id: &str| {
                        self.tools.iter().any(|entry| {
                            entry.definition.id == id && entry.definition.kind == ToolKind::Lsp
                        })
                    };
                    self.lsp_nix_workspace_preferred = state
                        .metadata
                        .iter()
                        .filter_map(|(key, value)| {
                            let id = key.strip_prefix("lsp.runtime.")?;
                            (value == "nix" && valid_lsp(id)).then(|| id.to_owned())
                        })
                        .collect();
                    self.lsp_managed_preferred = state
                        .metadata
                        .iter()
                        .filter_map(|(key, value)| {
                            let id = key.strip_prefix("lsp.runtime.")?;
                            (value == "managed" && valid_lsp(id)).then(|| id.to_owned())
                        })
                        .collect();
                    self.lsp_managed_versions = state
                        .metadata
                        .iter()
                        .filter_map(|(key, version)| {
                            let id = key.strip_prefix("lsp.version.")?;
                            self.tools
                                .iter()
                                .find(|entry| {
                                    entry.definition.id == id
                                        && entry.definition.kind == ToolKind::Lsp
                                })
                                .and_then(|entry| entry.definition.managed_version(version))
                                .and_then(|definition| {
                                    definition.version.map(|version| (id.to_owned(), version))
                                })
                        })
                        .collect();
                    if let Some(session) = &session
                        && let Err(diagnostic) =
                            self.apply_persisted_window_rules(&session.window_rules)
                    {
                        self.ui.config_diagnostic = Some(format!(
                            "persistent window layout rejected transactionally: {diagnostic}"
                        ));
                    }
                    let configured = &self.configuration.settings.persistence;
                    self.recent_files = state
                        .recent_files
                        .into_iter()
                        .take(configured.recent_files)
                        .collect();
                    self.marks = state.marks.into_iter().take(configured.marks).collect();
                    self.jump_list = state.jumps.into_iter().take(configured.jumps).collect();
                    self.change_list = state.changes.into_iter().take(configured.changes).collect();
                    self.dap_breakpoints.clear();
                    for row in
                        state
                            .breakpoints
                            .into_iter()
                            .take(self.configuration.settings.debug.maximum_breakpoints)
                            .filter(|row| {
                                row.path.is_absolute()
                                    && self.workspace.roots().iter().any(|root| {
                                        row.path.starts_with(&root.identity.canonical_path)
                                    })
                            })
                    {
                        self.dap_breakpoints
                            .entry(row.path.clone())
                            .or_default()
                            .push(DapSourceBreakpoint {
                                path: row.path,
                                line: row.line,
                                column: row.column,
                                condition: row.condition,
                                hit_condition: row.hit_condition,
                                log_message: row.log_message,
                            });
                    }
                    self.persisted_large_file_overrides = state
                        .large_file_overrides
                        .into_iter()
                        .filter_map(|entry| {
                            let mode = match entry.mode.as_str() {
                                "on" => LargeFileOverride::On,
                                "off" => LargeFileOverride::Off,
                                "auto" => LargeFileOverride::Auto,
                                _ => return None,
                            };
                            Some((entry.path, mode))
                        })
                        .collect();
                    self.dap_watches = state
                        .watches
                        .into_iter()
                        .take(self.configuration.settings.debug.maximum_watches)
                        .map(|watch| watch.expression)
                        .collect();
                    if configured.persist_command_history {
                        self.command_history = state.command_history;
                    }
                    if configured.persist_search_history {
                        self.search_history = state.search_history;
                    }
                    self.persistence_pending = None;
                    let command_excess = self
                        .command_history
                        .len()
                        .saturating_sub(configured.maximum_entries);
                    self.command_history.drain(..command_excess);
                    let search_excess = self
                        .search_history
                        .len()
                        .saturating_sub(configured.maximum_entries);
                    self.search_history.drain(..search_excess);
                    if let Some(session) = session {
                        self.restore_session(session);
                    }
                }
                PersistenceEvent::Saved { generation }
                    if self.persistence_pending == Some(generation) =>
                {
                    self.persistence_pending = None
                }
                PersistenceEvent::Failed { generation, error }
                    if self.persistence_pending == Some(generation) =>
                {
                    self.persistence_pending = None;
                    self.ui.config_diagnostic =
                        Some(error.to_string().chars().take(1024).collect());
                }
                _ => {}
            }
        }
    }

    pub(super) fn restore_session(&mut self, session: PersistedSession) {
        if self.safe_mode {
            return;
        }
        if self.session_restore.is_some() {
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        let mut panes = Vec::new();
        for tab in &session.tabs {
            collect_persisted_panes(&tab.layout, &mut panes)
        }
        let paths = panes
            .into_iter()
            .map(|pane| pane.path)
            .filter(|path| roots.iter().any(|root| path.starts_with(root)))
            .take(self.configuration.settings.persistence.maximum_entries)
            .collect::<BTreeSet<_>>();
        if paths.is_empty() {
            return;
        }
        let mut waiting = 0;
        for path in &paths {
            if let Ok(id) = self.vfs.read_scoped(path.clone(), roots.clone()) {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::SessionRestore {
                        requested_path: path.clone(),
                    },
                );
                waiting += 1
            } else {
                return;
            }
        }
        self.session_restore = Some(PendingSessionRestore {
            session,
            waiting,
            loaded: HashMap::new(),
            failed: false,
        });
    }

    pub(super) fn finish_session_restore(&mut self) {
        let Some(restore) = self.session_restore.take() else {
            return;
        };
        if restore.failed || restore.waiting != 0 {
            return;
        }
        let mut candidate = EditorState::new(self.state.workspace_root.clone());
        candidate.policy = self.state.policy;
        let mut tabs = Vec::new();
        for tab in &restore.session.tabs {
            let Some((layout, panes)) =
                build_restored_layout(&mut candidate, &tab.layout, &restore.loaded)
            else {
                return;
            };
            let Some(focused) = tab
                .focused_path
                .as_ref()
                .and_then(|path| {
                    panes
                        .iter()
                        .find(|(candidate, _)| candidate == path)
                        .map(|(_, pane)| *pane)
                })
                .or_else(|| panes.first().map(|(_, pane)| *pane))
            else {
                return;
            };
            let tab_id = candidate.tabs.insert(crate::editor::Tab {
                root: layout,
                focused,
            });
            tabs.push((tab_id, focused));
        }
        if tabs.is_empty() {
            return;
        }
        let active = restore.session.active_tab.min(tabs.len() - 1);
        candidate.active_tab = Some(tabs[active].0);
        candidate.active_view = candidate.panes.get(tabs[active].1).map(|pane| pane.view);
        candidate.mark_window_changed();
        if candidate.validate_layout().is_err() {
            return;
        }
        let mut graph = self.windows.clone();
        let mut plugin_windows = self.plugin_windows.clone();
        let mut surface_windows = self.surface_windows.clone();
        let mut editor_windows = self.editor_windows.clone();
        let reset = plugin_windows
            .reset_runtime(&mut graph)
            .and_then(|()| surface_windows.reset(&mut graph))
            .and_then(|()| editor_windows.reset(&mut graph));
        if let Err(error) = reset {
            self.logs.record(
                LogLevel::Error,
                "windows",
                "session-window-reset-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            );
            return;
        }
        self.windows = graph;
        self.plugin_windows = plugin_windows;
        self.surface_windows = surface_windows;
        self.editor_windows = editor_windows;
        self.pending_plugin_ui_events.clear();
        self.pending_plugin_storage_results.clear();
        let storage_generations = self
            .plugin_contributions
            .ordered()
            .filter(|index| index.storage.is_some())
            .map(|index| (index.owner.clone(), PluginLoadId(index.generation)))
            .collect::<Vec<_>>();
        for (plugin, load) in storage_generations {
            self.plugin_storage.reset_generation(&plugin, load);
        }
        self.plugin_host_events.reset_runtime();
        self.plugin_host_event_observation = None;
        self.plugin_providers.reset_runtime();
        self.plugin_virtual_documents.reset_runtime();
        self.plugin_virtual_editor = VirtualDocumentEditorBridge::default();
        self.active_plugin_picker = None;
        self.ui.plugin_surfaces.clear();
        self.ui.private_surfaces.clear();
        self.state = candidate;
        self.ui.dashboard = false;
        for (canonical, _) in restore.loaded.values() {
            self.record_recent_file(canonical.clone());
        }
        self.ensure_cursor_visible();
        self.reconcile_editor_windows();
    }

    pub(super) fn fail_session_restore(&mut self) {
        if let Some(restore) = &mut self.session_restore {
            restore.failed = true;
            restore.waiting = restore.waiting.saturating_sub(1);
            if restore.waiting == 0 {
                self.session_restore = None
            }
        }
    }

    pub(super) fn accept_session_restore_read(
        &mut self,
        requested: PathBuf,
        canonical: PathBuf,
        text: TextBuffer,
    ) {
        let mut finish = false;
        if let Some(restore) = &mut self.session_restore {
            restore.loaded.insert(requested, (canonical, text));
            restore.waiting = restore.waiting.saturating_sub(1);
            finish = restore.waiting == 0;
        }
        if finish {
            self.finish_session_restore();
        }
    }

    pub(super) fn record_recent_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|candidate| candidate != &path);
        self.recent_files.insert(0, path);
        self.recent_files
            .truncate(self.configuration.settings.persistence.recent_files);
    }
}

pub(in crate::editor::session) fn mode_name_large(mode: LargeFileOverride) -> &'static str {
    match mode {
        LargeFileOverride::Auto => "auto",
        LargeFileOverride::On => "on",
        LargeFileOverride::Off => "off",
    }
}

pub(in crate::editor::session) fn build_restored_layout(
    state: &mut EditorState,
    layout: &PersistedLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::editor::LayoutNode,
    Vec<(PathBuf, crate::editor::PaneId)>,
)> {
    match layout {
        PersistedLayout::Pane { pane } => {
            let (canonical, text) = loaded.get(&pane.path)?;
            let buffer = state.load_hidden_text(text.clone(), canonical.clone());
            let mut view = crate::editor::View::new(buffer);
            let length = state.buffers[buffer].text.len_bytes();
            view.cursor = ByteOffset(pane.cursor.min(length));
            view.scroll_line = pane
                .scroll_line
                .min(state.buffers[buffer].text.len_lines().saturating_sub(1));
            view.scroll_wrap_row = 0;
            view.scroll_column = pane.scroll_column;
            if pane.anchor != pane.cursor {
                view.selection = Some(crate::editor::Selection {
                    anchor: ByteOffset(pane.anchor.min(length)),
                    head: view.cursor,
                    kind: crate::editor::SelectionKind::Character,
                });
            }
            let view = state.views.insert(view);
            let pane_id = state.panes.insert(crate::editor::Pane { view });
            Some((
                crate::editor::LayoutNode::Pane(pane_id),
                vec![(pane.path.clone(), pane_id)],
            ))
        }
        PersistedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !(100..=900).contains(ratio) {
                return None;
            }
            let (first, mut panes) = build_restored_layout(state, first, loaded)?;
            let (second, second_panes) = build_restored_layout(state, second, loaded)?;
            panes.extend(second_panes);
            let axis = match axis.as_str() {
                "horizontal" => crate::editor::Axis::Horizontal,
                "vertical" => crate::editor::Axis::Vertical,
                _ => return None,
            };
            Some((
                crate::editor::LayoutNode::Split {
                    axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                },
                panes,
            ))
        }
    }
}

pub(in crate::editor::session) fn collect_persisted_panes(
    layout: &PersistedLayout,
    panes: &mut Vec<PersistedPane>,
) {
    match layout {
        PersistedLayout::Pane { pane } => panes.push(pane.clone()),
        PersistedLayout::Split { first, second, .. } => {
            collect_persisted_panes(first, panes);
            collect_persisted_panes(second, panes);
        }
    }
}

pub(in crate::editor::session) fn capture_persisted_layout(
    state: &EditorState,
    layout: &crate::editor::LayoutNode,
) -> Option<PersistedLayout> {
    match layout {
        crate::editor::LayoutNode::Pane(pane) => {
            let pane = state.panes.get(*pane)?;
            let view = state.views.get(pane.view)?;
            let buffer = state.buffers.get(view.buffer)?;
            if buffer.dirty {
                return None;
            }
            let path = buffer.path.clone()?;
            Some(PersistedLayout::Pane {
                pane: PersistedPane {
                    path,
                    cursor: view.cursor.get(),
                    anchor: view
                        .selection
                        .map_or(view.cursor, |selection| selection.anchor)
                        .get(),
                    scroll_line: view.scroll_line,
                    scroll_column: view.scroll_column,
                },
            })
        }
        crate::editor::LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = capture_persisted_layout(state, first);
            let second = capture_persisted_layout(state, second);
            match (first, second) {
                (Some(first), Some(second)) => Some(PersistedLayout::Split {
                    axis: match axis {
                        crate::editor::Axis::Horizontal => "horizontal",
                        crate::editor::Axis::Vertical => "vertical",
                    }
                    .into(),
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(layout), None) | (None, Some(layout)) => Some(layout),
                (None, None) => None,
            }
        }
    }
}
