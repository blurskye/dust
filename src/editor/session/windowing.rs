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
            .and_then(|_| {
                explorer_panel_width(self.configuration.settings(), self.window_session.width())
            })
            .map_or(1, |width| width.saturating_add(1));
        let terminal_size = self
            .displayed_terminal()
            .and_then(|_| {
                terminal_panel_geometry(
                    self.configuration.settings(),
                    self.window_session.height(),
                    self.window_session.terminal_height(),
                )
            })
            .map_or(1, |geometry| geometry.height);
        let picker_rect = self.ui.picker.as_ref().and_then(|picker| {
            crate::ui::geometry::picker_window_geometry(
                picker,
                self.configuration.settings(),
                WindowRect {
                    x: 0,
                    y: 0,
                    width: self.window_session.width(),
                    height: self.window_session.height(),
                },
            )
        });
        NativeSurfaceSignature {
            editor_tab: editor.active_tab(&(&self.state).into()),
            editor_window: editor.focused_window(&(&self.state).into()),
            explorer_visible: self.ui.explorer.is_some(),
            explorer_focused: self.ui.explorer.is_some() && self.window_session.explorer_focused(),
            explorer_size,
            active_terminal: self.displayed_terminal().map(|id| id.0),
            terminal_focused: self.displayed_terminal().is_some_and(|id| {
                self.window_session.focused_terminal() == Some(id.0)
                    && self.window_session.native_ids().terminal_placement(id.0)
                        != Some(WindowPlacement::Hidden)
            }),
            terminal_size,
            terminal_set_revision: self.terminal_session.revision(),
            terminal_width: self.window_session.width(),
            terminal_height: self.window_session.height(),
            dashboard_visible: self.window_session.dashboard_visible()
                || self.state.active_view.is_none(),
            picker_rect,
            picker_surface: self
                .ui
                .picker
                .as_ref()
                .filter(|_| picker_rect.is_some())
                .map(|picker| picker.surface),
            protected_prompt_height: self
                .input
                .prompt()
                .map_or(0, |prompt| if prompt.error.is_some() { 4 } else { 3 }),
            configuration_generation: self.configuration.generation(),
        }
    }

    pub(super) fn reconcile_editor_windows(&mut self) {
        let scratch_label = &self.configuration.settings().ui.buffer_line.scratch_label;
        let editor_current = self
            .window_session
            .editor_ids()
            .is_current(&(&self.state).into(), scratch_label);
        let signature = self.native_surface_signature(self.window_session.editor_ids());
        if editor_current
            && self.window_session.native_ids().is_current(signature)
            && self.window_session.graph().revision() == self.ui.window_graph_revision
            && self
                .window_session
                .content_current(self.configuration.generation())
            && self.compositor_projection().is_some()
        {
            return;
        }
        let result = if editor_current && self.window_session.native_ids().is_current(signature) {
            Ok(())
        } else {
            let terminals = self.terminal_session.descriptions();
            self.window_session
                .prepare_reconcile(
                    &(&self.state).into(),
                    scratch_label,
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
                .and_then(|candidate| {
                    self.window_session
                        .accept_current(candidate, &self.state.window_version())
                })
        };
        match result {
            Ok(()) => {
                self.project_window_content_tabs();
                let graph = self.window_session.graph();
                let bridge = self.window_session.editor_ids();
                let surfaces = self.window_session.native_ids();
                self.ui.editor_window_layout = self.state.window_revision().and_then(|revision| {
                    bridge
                        .active_layout(&(&self.state).into(), graph)
                        .map(|active| EditorWindowLayoutState {
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
                        })
                });
                self.ui.window_graph_revision = graph.revision();
                let projection = self.state.window_revision().and_then(|revision| {
                    let graph_revision = graph.revision()?;
                    let tab = bridge
                        .active_tab(&(&self.state).into())
                        .or_else(|| surfaces.workspace_tab())?;
                    let snapshot = graph.compositor_snapshot(tab)?;
                    let preliminary = snapshot.project(
                        WindowRect {
                            x: 0,
                            y: 0,
                            width: self.window_session.width(),
                            height: self.window_session.height(),
                        },
                        self.configuration.settings().ui.panes.minimum_width,
                        self.configuration.settings().ui.panes.minimum_height,
                    );
                    let (top, bottom) = editor_workspace_insets(
                        &self.state,
                        &self.ui,
                        self.configuration.settings(),
                        preliminary.workspace_bounds.height,
                    );
                    let protected = self.input.prompt().map(|prompt| {
                        let geometry = prompt_geometry(
                            self.window_session.width(),
                            preliminary.workspace_bounds.height,
                            &self.configuration.settings().ui.command_line,
                            prompt.error.is_some() || prompt.help.is_some(),
                        );
                        WindowRect {
                            x: geometry.left,
                            y: geometry.top,
                            width: geometry.width,
                            height: geometry.height,
                        }
                    });
                    Some(ProjectionRequest {
                        tab,
                        source_revision: revision,
                        graph_revision,
                        bounds: preliminary.bounds,
                        minimum_size: (
                            self.configuration.settings().ui.panes.minimum_width,
                            self.configuration.settings().ui.panes.minimum_height,
                        ),
                        insets: (top, bottom),
                        protected,
                        surfaces: NativeSurfaceWindowBinding {
                            explorer: surfaces.explorer().map(|ids| ids.window),
                            terminal: self.displayed_terminal().and_then(|id| {
                                surfaces.terminal(id.0).map(|ids| (id.0, ids.window))
                            }),
                            dashboard: surfaces.dashboard().map(|ids| ids.window),
                            picker: surfaces.picker().map(|ids| ids.window),
                        },
                    })
                });
                if let Some(projection) = projection {
                    self.window_session.project(projection);
                }
                self.project_window_geometry();
                self.sync_explorer_ui();
                self.sync_plugin_window_ui();
            }
            Err(error) => self.notices.record(
                LogLevel::Error,
                "windows",
                "window-reconciliation-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            ),
        }
    }
}
