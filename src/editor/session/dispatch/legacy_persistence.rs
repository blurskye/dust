use super::*;

impl InteractiveApplication {
    pub(super) fn effective_large_file(&self, buffer: crate::editor::BufferId) -> bool {
        self.documents.effective_large_file(
            buffer,
            self.state.buffers.get(buffer).map(|buffer| &buffer.text),
            &self.configuration.settings().editor.large_file,
        )
    }
    pub(super) fn process_large_file_modes(&mut self) {
        let large = self.documents.reconcile_policy(
            &self.state.window_version(),
            self.state
                .buffers
                .iter()
                .map(|(id, buffer)| (id, buffer.path.as_deref(), &buffer.text)),
            &self.configuration.settings().editor.large_file,
        );
        self.project_large_file_modes(large);
    }

    pub(super) fn persistence_policy(&self) -> PersistencePolicy {
        PersistencePolicy {
            maximum_file_bytes: self.configuration.settings().persistence.maximum_file_bytes,
            maximum_entries: self.configuration.settings().persistence.maximum_entries,
        }
    }

    pub(super) fn load_persistence(&mut self) {
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled || !configured.restore_on_startup {
            return;
        }
        if let Err(error) = self.persistence.load(self.persistence_policy()) {
            self.ui.config_diagnostic = Some(error);
        }
    }

    pub(super) fn capture_window_rules(&self) -> Vec<PersistedWindowRule> {
        let mut rules = self
            .window_session
            .native_ids()
            .semantic_defaults(self.terminal_session.selected().map(|id| id.0));
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.explorer")
            && let Some(ids) = self.window_session.native_ids().explorer()
            && let Some(window) = self.window_session.graph().window(ids.window)
        {
            rules.push(("builtin.explorer", window.placement));
        }
        if !rules
            .iter()
            .any(|(content_type, _)| *content_type == "builtin.terminal")
            && let Some(ids) = self
                .terminal_session
                .selected()
                .and_then(|id| self.window_session.native_ids().terminal(id.0))
            && let Some(window) = self.window_session.graph().window(ids.window)
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
    ) -> Result<WindowCandidate, String> {
        let terminals = self.terminal_session.descriptions();
        self.window_session.prepare_rules(
            windows,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )
    }

    pub(super) fn apply_persisted_window_rules(
        &mut self,
        rules: &[PersistedWindowRule],
    ) -> Result<(), String> {
        validate_window_rules(rules).map_err(|error| error.to_string())?;
        let rules = rules
            .iter()
            .map(|rule| {
                Ok((
                    rule.content_type.as_str(),
                    restored_window_placement(&rule.placement)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let terminals = self.terminal_session.descriptions();
        let candidate = self.window_session.prepare_semantic_defaults(
            &rules,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )?;
        self.window_session
            .accept_current(candidate, &self.state.window_version())?;
        let requires_projection = self.window_session.graph().active_tab().is_some();
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
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled || !configured.save_on_exit || self.persistence.pending() {
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
                self.input.command_history().to_vec()
            } else {
                Vec::new()
            },
            search_history: if configured.persist_search_history {
                self.input.search_history().to_vec()
            } else {
                Vec::new()
            },
            breakpoints: self.debugger_session.persisted_breakpoints(),
            large_file_overrides: self
                .documents
                .policy_overrides()
                .filter_map(|(id, mode)| {
                    let path = self.state.buffers.get(id)?.path.clone()?;
                    Some(PersistedLargeFileOverride {
                        path,
                        mode: mode_name_large(mode).into(),
                    })
                })
                .take(configured.maximum_entries)
                .collect(),
            watches: self.debugger_session.persisted_watches(),
            metadata: self.tooling.metadata(),
        };
        if let Err(error) = self.persistence.save(state, self.persistence_policy()) {
            self.ui.config_diagnostic = Some(error);
        }
    }

    pub(super) fn process_persistence_events(&mut self) {
        while let Some(event) = self.persistence.poll() {
            match event {
                Ok(state) => {
                    let session = state.session.clone();
                    self.tooling.restore(&state.metadata);
                    if let Some(session) = &session
                        && let Err(diagnostic) =
                            self.apply_persisted_window_rules(&session.window_rules)
                    {
                        self.ui.config_diagnostic = Some(format!(
                            "persistent window layout rejected transactionally: {diagnostic}"
                        ));
                    }
                    let configured = &self.configuration.settings().persistence;
                    self.recent_files = state
                        .recent_files
                        .into_iter()
                        .take(configured.recent_files)
                        .collect();
                    self.marks = state.marks.into_iter().take(configured.marks).collect();
                    self.jump_list = state.jumps.into_iter().take(configured.jumps).collect();
                    self.change_list = state.changes.into_iter().take(configured.changes).collect();
                    self.debugger_session.restore(
                        state.breakpoints,
                        state.watches,
                        &self.workspace,
                    );
                    self.documents.restore_policy(
                        state
                            .large_file_overrides
                            .into_iter()
                            .map(|entry| (entry.path, entry.mode)),
                    );
                    self.input.restore_history(
                        state.command_history,
                        state.search_history,
                        configured,
                    );
                    if let Some(session) = session {
                        self.restore_session(session);
                    }
                }
                Err(error) => self.ui.config_diagnostic = Some(error),
            }
        }
    }

    pub(super) fn restore_session(&mut self, session: PersistedSession) {
        if self.workspace.safe_mode() {
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
            .take(self.configuration.settings().persistence.maximum_entries)
            .collect::<BTreeSet<_>>();
        if paths.is_empty() {
            return;
        }
        let mut waiting = 0;
        for path in &paths {
            if let Ok(id) = self.documents.read_scoped(path.clone(), roots.clone()) {
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
        let reset = self
            .window_session
            .reset_runtime(&self.plugin_windows.generations());
        if let Err(error) = reset {
            self.notices.record(
                LogLevel::Error,
                "windows",
                "session-window-reset-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            );
            return;
        }
        self.plugin_windows.clear_runtime();
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
        self.set_dashboard_visibility(false);
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
            .truncate(self.configuration.settings().persistence.recent_files);
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

pub(in crate::editor::session) fn persisted_window_placement(
    placement: WindowPlacement,
) -> Option<PersistedWindowPlacement> {
    match placement {
        WindowPlacement::Docked { edge, size } => Some(PersistedWindowPlacement::Docked {
            edge: match edge {
                DockEdge::Left => "left",
                DockEdge::Right => "right",
                DockEdge::Top => "top",
                DockEdge::Bottom => "bottom",
            }
            .into(),
            size,
        }),
        WindowPlacement::Floating(rect) => Some(PersistedWindowPlacement::Floating {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }),
        WindowPlacement::Fullscreen => Some(PersistedWindowPlacement::Fullscreen),
        WindowPlacement::Hidden => Some(PersistedWindowPlacement::Hidden),
        WindowPlacement::Tiled | WindowPlacement::Overlay(_) => None,
    }
}

pub(in crate::editor::session) fn restored_window_placement(
    placement: &PersistedWindowPlacement,
) -> Result<WindowPlacement, String> {
    let placement = match placement {
        PersistedWindowPlacement::Docked { edge, size } => WindowPlacement::Docked {
            edge: match edge.as_str() {
                "left" => DockEdge::Left,
                "right" => DockEdge::Right,
                "top" => DockEdge::Top,
                "bottom" => DockEdge::Bottom,
                _ => return Err(format!("unknown dock edge `{edge}`")),
            },
            size: *size,
        },
        PersistedWindowPlacement::Floating {
            x,
            y,
            width,
            height,
        } => WindowPlacement::Floating(WindowRect {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        }),
        PersistedWindowPlacement::Fullscreen => WindowPlacement::Fullscreen,
        PersistedWindowPlacement::Hidden => WindowPlacement::Hidden,
    };
    placement.validate().map_err(|error| error.to_string())
}
