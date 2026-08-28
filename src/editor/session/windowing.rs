use super::*;

impl InteractiveApplication {
    pub(super) fn native_surface_signature(
        &self,
        editor: &EditorWindowBridge,
    ) -> NativeSurfaceSignature {
        let explorer_size = self
            .ui
            .explorer
            .as_ref()
            .and_then(|_| explorer_panel_width(&self.configuration.settings, self.terminal_width))
            .map_or(1, |width| width.saturating_add(1));
        let terminal_size = self
            .active_terminal
            .and_then(|_| {
                terminal_panel_geometry(
                    &self.configuration.settings,
                    self.terminal_height,
                    self.terminal_panel_height,
                )
            })
            .map_or(1, |geometry| geometry.height);
        NativeSurfaceSignature {
            editor_tab: editor.active_tab(&self.state),
            editor_window: editor.focused_window(&self.state),
            explorer_visible: self.ui.explorer.is_some(),
            explorer_focused: self.ui.explorer.is_some() && self.ui.explorer_focused,
            explorer_size,
            active_terminal: self.active_terminal.map(|id| id.0),
            terminal_focused: self.active_terminal.is_some_and(|id| {
                self.terminal_focused
                    && self.surface_windows.terminal_placement(id.0)
                        != Some(WindowPlacement::Hidden)
            }),
            terminal_size,
            terminal_set_revision: self.terminal_set_revision,
            terminal_width: self.terminal_width,
            terminal_height: self.terminal_height,
            dashboard_visible: self.ui.dashboard || self.state.active_view.is_none(),
            picker_rect: self.ui.picker.as_ref().and_then(|_| {
                picker_geometry(
                    &self.configuration.settings,
                    WindowRect {
                        x: 0,
                        y: 0,
                        width: self.terminal_width,
                        height: self.terminal_height,
                    },
                )
            }),
            picker_surface: self.ui.picker.as_ref().and_then(|picker| {
                picker_geometry(
                    &self.configuration.settings,
                    WindowRect {
                        x: 0,
                        y: 0,
                        width: self.terminal_width,
                        height: self.terminal_height,
                    },
                )
                .map(|_| picker.surface)
            }),
            protected_prompt_height: self
                .ui
                .prompt
                .as_ref()
                .map_or(0, |prompt| if prompt.error.is_some() { 4 } else { 3 }),
            configuration_generation: self.configuration_generation,
        }
    }

    pub(super) fn reconcile_editor_windows(&mut self) {
        let scratch_label = &self.configuration.settings.ui.buffer_line.scratch_label;
        let editor_current = self.editor_windows.is_current(&self.state, scratch_label);
        if editor_current {
            let signature = self.native_surface_signature(&self.editor_windows);
            if self.surface_windows.is_current(signature)
                && self.windows.revision() == self.ui.window_graph_revision
                && self.content_line_projection_signature
                    == self.windows.revision().map(|revision| {
                        (
                            revision,
                            self.content_line_diagnostic_revision,
                            self.content_line_order_revision,
                            self.configuration_generation,
                        )
                    })
                && self.ui.compositor_layout.as_ref().is_some_and(|layout| {
                    Some(layout.graph_revision) == self.windows.revision()
                        && Some(layout.source_revision) == self.state.window_revision()
                })
            {
                return;
            }
        }
        let mut graph = self.windows.clone();
        let mut bridge = self.editor_windows.clone();
        let mut surfaces = self.surface_windows.clone();
        let result = (|| {
            let mut terminals = None;
            if !editor_current {
                let pre_signature = self.native_surface_signature(&bridge);
                if !surfaces.is_current(pre_signature) {
                    let mut rows = self
                        .terminals
                        .iter()
                        .map(|(id, terminal)| (id.0, terminal.profile.clone()))
                        .collect::<Vec<_>>();
                    rows.sort_unstable_by_key(|(id, _)| *id);
                    surfaces.reconcile(
                        &mut graph,
                        pre_signature,
                        &self.message("explorer_title", &[]),
                        &self.configuration.dashboard.identity,
                        self.ui
                            .picker
                            .as_ref()
                            .map_or("Picker", |picker| &picker.title),
                        &rows,
                    )?;
                    terminals = Some(rows);
                }
                bridge.reconcile(&self.state, &mut graph, scratch_label)?;
            }
            let signature = self.native_surface_signature(&bridge);
            if !editor_current || !surfaces.is_current(signature) {
                let rows = terminals.get_or_insert_with(|| {
                    let mut rows = self
                        .terminals
                        .iter()
                        .map(|(id, terminal)| (id.0, terminal.profile.clone()))
                        .collect::<Vec<_>>();
                    rows.sort_unstable_by_key(|(id, _)| *id);
                    rows
                });
                surfaces.reconcile(
                    &mut graph,
                    signature,
                    &self.message("explorer_title", &[]),
                    &self.configuration.dashboard.identity,
                    self.ui
                        .picker
                        .as_ref()
                        .map_or("Picker", |picker| &picker.title),
                    rows,
                )?;
            }
            graph.validate()?;
            Ok::<(), crate::ui::window::WindowError>(())
        })();
        match result {
            Ok(()) => {
                self.ui.editor_window_layout = self.state.window_revision().and_then(|revision| {
                    bridge.active_layout(&self.state, &graph).map(|active| {
                        EditorWindowLayoutState {
                            source_revision: revision,
                            layout: active.layout,
                            bindings: active
                                .bindings
                                .into_iter()
                                .map(|binding| UiEditorWindowBinding {
                                    window: binding.window,
                                    pane: binding.pane,
                                    view: binding.view,
                                })
                                .collect(),
                        }
                    })
                });
                let graph_revision = graph.revision().unwrap_or(u64::MAX);
                let signature = (
                    graph_revision,
                    self.content_line_diagnostic_revision,
                    self.content_line_order_revision,
                    self.configuration_generation,
                );
                if self.content_line_projection_signature != Some(signature) {
                    let snapshots = graph
                        .content_snapshots()
                        .into_iter()
                        .filter(|content| {
                            matches!(
                                content.adapter,
                                crate::ui::window::ContentAdapterKind::TextBuffer
                                    | crate::ui::window::ContentAdapterKind::VirtualDocument
                            ) || matches!(
                                content.adapter,
                                crate::ui::window::ContentAdapterKind::DeclarativeUi
                                    | crate::ui::window::ContentAdapterKind::PrivateCellSurface
                            ) && (content.content_type.as_str().starts_with("plugin.")
                                || matches!(
                                    content.content_type.as_str(),
                                    "builtin.problems"
                                        | "builtin.output"
                                        | "builtin.logs"
                                        | "builtin.debug"
                                ))
                        })
                        .collect::<Vec<_>>();
                    let represented = snapshots
                        .iter()
                        .filter_map(|content| bridge.buffer_for_content(content.id))
                        .collect::<BTreeSet<_>>();
                    let before_order = self.content_line_order.clone();
                    let before_pins = self.content_line_pins.clone();
                    self.content_line_order
                        .retain(|buffer| represented.contains(buffer));
                    self.content_line_pins
                        .retain(|buffer| represented.contains(buffer));
                    for content in &snapshots {
                        if let Some(buffer) = bridge.buffer_for_content(content.id)
                            && !self.content_line_order.contains(&buffer)
                        {
                            self.content_line_order.push(buffer)
                        }
                    }
                    if before_order != self.content_line_order
                        || before_pins != self.content_line_pins
                    {
                        self.content_line_order_revision =
                            self.content_line_order_revision.saturating_add(1);
                    }
                    let mut entries = snapshots
                        .into_iter()
                        .enumerate()
                        .map(|(native_index, content)| {
                            let buffer = bridge.buffer_for_content(content.id);
                            let pinned = buffer
                                .is_some_and(|buffer| self.content_line_pins.contains(&buffer));
                            let order = buffer
                                .and_then(|buffer| {
                                    self.content_line_order
                                        .iter()
                                        .position(|candidate| *candidate == buffer)
                                })
                                .unwrap_or(
                                    self.content_line_order.len().saturating_add(native_index),
                                );
                            let (icon, diagnostic, closeable) = buffer.map_or_else(
                                || {
                                    let icon = match content.adapter {
                                        crate::ui::window::ContentAdapterKind::TerminalSession
                                        | crate::ui::window::ContentAdapterKind::ExternalTui => {
                                            self.configuration
                                                .settings
                                                .ui
                                                .buffer_line
                                                .terminal_icon
                                                .clone()
                                        }
                                        crate::ui::window::ContentAdapterKind::VirtualDocument => {
                                            self.configuration
                                                .settings
                                                .ui
                                                .buffer_line
                                                .virtual_icon
                                                .clone()
                                        }
                                        _ => self
                                            .configuration
                                            .settings
                                            .ui
                                            .buffer_line
                                            .custom_icon
                                            .clone(),
                                    };
                                    (icon, None, false)
                                },
                                |buffer| {
                                    let state = &self.state.buffers[buffer];
                                    let icon = if state.is_virtual_read_only() {
                                        self.configuration
                                            .settings
                                            .ui
                                            .buffer_line
                                            .virtual_icon
                                            .clone()
                                    } else if let Some(extension) = state
                                        .path
                                        .as_ref()
                                        .and_then(|path| path.extension())
                                        .and_then(|value| value.to_str())
                                        .map(str::to_ascii_lowercase)
                                    {
                                        self.configuration
                                            .settings
                                            .ui
                                            .buffer_line
                                            .extension_icons
                                            .get(&extension)
                                            .cloned()
                                            .unwrap_or_else(|| {
                                                self.configuration
                                                    .settings
                                                    .ui
                                                    .buffer_line
                                                    .custom_icon
                                                    .clone()
                                            })
                                    } else {
                                        self.configuration
                                            .settings
                                            .ui
                                            .buffer_line
                                            .scratch_icon
                                            .clone()
                                    };
                                    let diagnostic = state.path.as_ref().and_then(|path| {
                                        let matches = self
                                            .ui
                                            .diagnostics
                                            .iter()
                                            .filter(|diagnostic| &diagnostic.path == path)
                                            .collect::<Vec<_>>();
                                        if matches.is_empty() {
                                            None
                                        } else {
                                            let severity = if matches.iter().any(|diagnostic| {
                                                diagnostic.severity == DiagnosticSeverity::Error
                                            }) {
                                                ContentTabDiagnosticSeverity::Error
                                            } else {
                                                ContentTabDiagnosticSeverity::Warning
                                            };
                                            Some(ContentTabDiagnostic {
                                                severity,
                                                count: u16::try_from(matches.len())
                                                    .unwrap_or(u16::MAX),
                                            })
                                        }
                                    });
                                    (icon, diagnostic, true)
                                },
                            );
                            let entry = ContentTabEntry {
                                identity: ContentTabIdentity(content.id),
                                kind: if buffer.is_some_and(|buffer| {
                                    self.state.buffers[buffer].is_virtual_read_only()
                                }) {
                                    ContentTabKind::VirtualDocument
                                } else {
                                    ContentTabKind::from(content.adapter)
                                },
                                title: content.title,
                                active: content.active,
                                dirty: content.dirty,
                                pinned,
                                diagnostic,
                                icon,
                                closeable,
                            };
                            (
                                (
                                    if buffer.is_some() {
                                        u8::from(!pinned)
                                    } else {
                                        2
                                    },
                                    order,
                                ),
                                entry,
                            )
                        })
                        .collect::<Vec<_>>();
                    entries.sort_by_key(|(key, _)| *key);
                    self.ui.content_tabs = entries.into_iter().map(|(_, entry)| entry).collect();
                    self.content_line_projection_signature = Some((
                        graph_revision,
                        self.content_line_diagnostic_revision,
                        self.content_line_order_revision,
                        self.configuration_generation,
                    ));
                }
                self.ui.window_graph_revision = graph.revision();
                self.ui.compositor_layout = self.state.window_revision().and_then(|revision| {
                    let graph_revision = graph.revision()?;
                    let tab = bridge
                        .active_tab(&self.state)
                        .or_else(|| surfaces.workspace_tab())?;
                    let snapshot = graph.compositor_snapshot(tab)?;
                    let preliminary = snapshot.project(
                        WindowRect {
                            x: 0,
                            y: 0,
                            width: self.terminal_width,
                            height: self.terminal_height,
                        },
                        self.configuration.settings.ui.panes.minimum_width,
                        self.configuration.settings.ui.panes.minimum_height,
                    );
                    let (top, bottom) = editor_workspace_insets(
                        &self.state,
                        &self.ui,
                        &self.configuration.settings,
                        preliminary.workspace_bounds.height,
                    );
                    let mut projection = snapshot.project_with_tiled_insets(
                        preliminary.bounds,
                        self.configuration.settings.ui.panes.minimum_width,
                        self.configuration.settings.ui.panes.minimum_height,
                        top,
                        bottom,
                    );
                    if let Some(prompt) = &self.ui.prompt {
                        let geometry = prompt_geometry(
                            self.terminal_width,
                            projection.workspace_bounds.height,
                            &self.configuration.settings.ui.command_line,
                            prompt.error.is_some(),
                        );
                        projection.set_protected_regions([WindowRect {
                            x: geometry.left,
                            y: geometry.top,
                            width: geometry.width,
                            height: geometry.height,
                        }]);
                    }
                    Some(CompositorLayoutState {
                        source_revision: revision,
                        graph_revision,
                        projection,
                        surfaces: NativeSurfaceWindowBinding {
                            explorer: surfaces.explorer().map(|ids| ids.window),
                            terminal: self.active_terminal.and_then(|id| {
                                surfaces.terminal(id.0).map(|ids| (id.0, ids.window))
                            }),
                            dashboard: surfaces.dashboard().map(|ids| ids.window),
                            picker: surfaces.picker().map(|ids| ids.window),
                        },
                    })
                });
                self.windows = graph;
                self.editor_windows = bridge;
                self.surface_windows = surfaces;
                self.sync_plugin_window_ui();
            }
            Err(error) => self.logs.record(
                LogLevel::Error,
                "windows",
                "window-reconciliation-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            ),
        }
    }
}
