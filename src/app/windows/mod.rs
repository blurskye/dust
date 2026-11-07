//! Checked window operations, native surfaces and accepted geometry.
pub(in crate::app) mod chrome;
pub(in crate::app) mod content;
mod focus;
mod native;
pub(in crate::app) use focus::FocusTarget;
mod mutations;
mod plans;
mod plugin;
use crate::config::Config;
use crate::ui::CompositorLayoutState;
use crate::ui::NativeSurfaceWindowBinding;
use crate::ui::PickerUiState;
use crate::ui::render::SurfaceContentGeometry;
use crate::ui::render::documentation::documentation_page_rows;
use crate::ui::render::explorer::explorer_body_geometry;
use crate::ui::render::picker::picker_content_geometry;
use crate::windows::CompositorProjection;
use crate::windows::ContentId;
use crate::windows::ViewId;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use crate::windows::WorkspaceTabId;
pub(in crate::app) use mutations::NativeDescriptions;
pub(in crate::app) use mutations::RestoreWindowPolicy;
pub(in crate::app) use native::NativeSurfaceSignature;
pub(in crate::app) use native::NativeSurfaceTarget;
use native::NativeSurfaces;
pub(in crate::app) use plans::CommandEffect;
pub(in crate::app) use plans::CommandError;
pub(in crate::app) use plans::CommandPolicy;
pub(in crate::app) use plans::EditorCloseSave;
pub(in crate::app) use plans::EditorCloseStep;
pub(in crate::app) use plans::EditorCloseTarget;
pub(in crate::app) use plans::FocusLease;
pub use plans::LayoutAction;
pub(in crate::app) use plans::LayoutPlanError;
pub(in crate::app) use plans::NavigationStamp;
pub(in crate::app) use plans::TargetError;
pub(in crate::app) use plans::WindowCommand;
pub(in crate::app) use plans::WindowPlan;
pub(in crate::app) use plugin::PluginWindowDraft;
use std::sync::Arc;

pub(super) struct ProjectionRequest {
    pub tab: WorkspaceTabId,
    pub source_revision: u64,
    pub graph_revision: u64,
    pub bounds: WindowRect,
    pub minimum_size: (u16, u16),
    pub insets: crate::windows::TiledInsets,
    pub surfaces: NativeSurfaceWindowBinding,
    pub protected: Option<WindowRect>,
}

#[derive(Clone, Debug)]
pub(in crate::app) struct TerminalGeometrySource {
    origin: Arc<()>,
    terminal: u64,
    content: ContentId,
    content_owner: String,
    content_instance_id: u64,
    view: ViewId,
    view_owner: String,
    view_instance_id: u64,
    window: WindowId,
    window_owner: String,
    window_instance_id: u64,
    tab: WorkspaceTabId,
    tab_owner: String,
    tab_instance_id: u64,
    placement: WindowPlacement,
}

impl PartialEq for TerminalGeometrySource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.origin, &other.origin)
            && self.terminal == other.terminal
            && self.content == other.content
            && self.content_owner == other.content_owner
            && self.content_instance_id == other.content_instance_id
            && self.view == other.view
            && self.view_owner == other.view_owner
            && self.view_instance_id == other.view_instance_id
            && self.window == other.window
            && self.window_owner == other.window_owner
            && self.window_instance_id == other.window_instance_id
            && self.tab == other.tab
            && self.tab_owner == other.tab_owner
            && self.tab_instance_id == other.tab_instance_id
            && self.placement == other.placement
    }
}
impl Eq for TerminalGeometrySource {}

#[cfg(test)]
impl TerminalGeometrySource {
    pub(in crate::app) fn fixture(instance_id: u64) -> Self {
        Self {
            origin: Arc::new(()),
            terminal: instance_id,
            content: ContentId(instance_id),
            content_owner: "fixture".into(),
            content_instance_id: instance_id,
            view: ViewId(instance_id),
            view_owner: "fixture".into(),
            view_instance_id: instance_id,
            window: WindowId(instance_id),
            window_owner: "fixture".into(),
            window_instance_id: instance_id,
            tab: WorkspaceTabId(instance_id),
            tab_owner: "fixture".into(),
            tab_instance_id: instance_id,
            placement: WindowPlacement::Hidden,
        }
    }
}

#[derive(Clone, Default)]
struct WindowState {
    // The last accepted graph is retained for projection and checked native drafts.
    graph: WindowGraph,
    source: Option<crate::editor::EditorWindowVersion>,
    scratch_label: Option<String>,
    native: NativeSurfaces,
}

pub(in crate::app) struct WindowCandidate {
    origin: Arc<()>,
    epoch: u64,
    core: WindowState,
    dashboard: Option<bool>,
}

pub(in crate::app) struct EditorWindowCandidate {
    proposal: crate::editor::EditorDraft,
    catalog: crate::plugins::PluginCatalogVersion,
    candidate: WindowCandidate,
}

#[derive(Default)]
pub(super) struct Windows {
    origin: Arc<()>,
    epoch: u64,
    content_line: content::ContentLine,
    chrome: crate::ui::render::chrome::ChromeLayout,
    named_segments: std::collections::BTreeMap<String, chrome::NamedSegment>,
    focus: FocusTarget,
    /// Where the keyboard was before the terminal took it, so hiding the terminal goes back there.
    before_terminal: FocusTarget,
    focus_stamp: Arc<()>,
    dashboard: bool,
    terminal_height: u16,
    terminal_shrunk: bool,
    debugging: bool,
    observed_size: (u16, u16),
    core: WindowState,
    layout: Option<CompositorLayoutState>,
    content: SurfaceContentGeometry,
    editor_close: Option<plans::EditorCloseIntent>,
}

impl Windows {
    #[cfg(test)]
    pub(in crate::app) fn test_publications(&self) -> u64 {
        self.epoch
    }

    pub(in crate::app) fn observe_size(&mut self, size: (u16, u16)) {
        if self.observed_size != size {
            self.observed_size = size;
            self.set_debugging(self.debugging);
            self.invalidate();
        }
    }
    pub(in crate::app) fn width(&self) -> u16 {
        self.observed_size.0
    }
    pub(in crate::app) fn height(&self) -> u16 {
        self.observed_size.1
    }

    pub(in crate::app) fn invalidate(&mut self) {
        self.layout = None;
        self.content = SurfaceContentGeometry::default();
    }

    pub(in crate::app) fn project(&mut self, request: ProjectionRequest) {
        if self.core.graph.revision() != Some(request.graph_revision) {
            self.invalidate();
            return;
        }
        let Some(snapshot) = self.core.graph.compositor_snapshot(request.tab) else {
            self.invalidate();
            return;
        };
        let mut projection = snapshot.project_with_tiled_insets(
            request.bounds,
            request.minimum_size.0,
            request.minimum_size.1,
            request.insets,
        );
        if let Some(protected) = request.protected {
            projection.set_protected_regions([protected]);
        }
        self.layout = Some(CompositorLayoutState {
            source_revision: request.source_revision,
            graph_revision: request.graph_revision,
            projection,
            surfaces: request.surfaces,
        });
        self.content = SurfaceContentGeometry::default();
    }

    pub(in crate::app) fn current_for_editor(
        &self,
        source: &crate::editor::EditorWindowVersion,
        size: (u16, u16),
    ) -> Option<&CompositorProjection> {
        if self.core.source.as_ref() != Some(source) {
            return None;
        }
        self.current(source.revision(), size)
    }
    pub(in crate::app) fn current(
        &self,
        source: Option<u64>,
        size: (u16, u16),
    ) -> Option<&CompositorProjection> {
        let layout = self.layout.as_ref()?;
        (source == Some(layout.source_revision)
            && self.core.graph.revision() == Some(layout.graph_revision)
            && size
                == (
                    layout.projection.bounds.width,
                    layout.projection.bounds.height,
                ))
            .then_some(&layout.projection)
    }

    pub(in crate::app) fn layout(&self) -> Option<&CompositorLayoutState> {
        self.layout.as_ref()
    }
    pub(in crate::app) fn content(&self) -> &SurfaceContentGeometry {
        &self.content
    }

    pub(in crate::app) fn project_content(
        &mut self,
        picker: Option<&PickerUiState>,
        config: &Config,
    ) {
        let Some(layout) = &self.layout else {
            return;
        };
        let rect = |window| layout.projection.window(window).map(|window| window.rect);
        self.content = SurfaceContentGeometry {
            picker: layout.surfaces.picker.and_then(rect).and_then(|rect| {
                picker.and_then(|picker| picker_content_geometry(picker, config, rect))
            }),
            explorer: layout
                .surfaces
                .explorer
                .and_then(rect)
                .map(explorer_body_geometry),
            terminal: layout
                .surfaces
                .terminal
                .and_then(|(_, window)| layout.projection.window(window))
                .and_then(crate::ui::terminal_window_geometry)
                .map(|geometry| WindowRect {
                    x: geometry.x,
                    y: geometry.body_top,
                    width: geometry.width,
                    height: geometry.body_height,
                }),
            documentation_rows: documentation_page_rows(
                config,
                layout.projection.workspace_bounds.width,
                layout.projection.workspace_bounds.height,
            ),
        };
    }
}

pub(in crate::app) fn rect_contains_point(rect: WindowRect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub(in crate::app) use plugin::PluginWindowKey;

pub(in crate::app) use plugin::PluginWindowResult;

pub(in crate::app) use plugin::PluginWindowTransition;

pub(in crate::app) mod commands;

#[cfg(test)]
mod viewport_tests;

use crate::app::App;
use crate::editor::EditorTransaction;
use crate::editor::UndoPolicy;
use crate::telemetry::LogLevel;
use crate::ui::PromptKind;
use crate::ui::render::SCRATCH_LABEL;
use crate::windows::DockEdge;
use crate::windows::SpatialDirection;
use std::collections::BTreeMap;
// Remaining editor/window routing; terminal data and algorithms live in the owner.
use crate::process::PtyId;

use crate::windows::PANE_DEFAULT_RATIO;
use crate::windows::PANE_MINIMUM_HEIGHT;
use crate::windows::PANE_MINIMUM_WIDTH;

use crate::ui::EditorWindowBinding as UiEditorWindowBinding;
use crate::ui::EditorWindowLayoutState;
use crate::ui::explorer_panel_width;
use crate::ui::render::TERMINAL_PANEL_RESIZE_STEP;
use crate::ui::screen_foot_rows;
use crate::ui::terminal_panel_geometry;
use crate::ui::workspace_top_rows;
use crate::windows::PANE_RESIZE_STEP;

impl App {
    pub(in crate::app) fn apply_window_plan(
        &mut self,
        plan: crate::app::windows::WindowPlan,
    ) -> Result<(), String> {
        self.window_session.check_plan(
            &plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
        )?;
        if plan.operations().is_empty() {
            self.window_session.accept_native_plan(
                plan,
                &self.state.window_version(),
                &self.extension.contributions().version(),
                &mut self.state,
            )?;
            if let Some(id) = self
                .window_session
                .focused_terminal()
                .map(PtyId)
                .filter(|id| self.terminal_session.contains(*id))
            {
                self.terminal_session.select(id);
            }
            self.project_window_activation();
            self.reconcile_editor_windows();
            return Ok(());
        }
        let transaction = plan
            .operations()
            .iter()
            .cloned()
            .fold(
                EditorTransaction::new(self.transaction_id()),
                |transaction, operation| transaction.with_operation(operation),
            )
            .with_undo_policy(UndoPolicy::Ignore);
        self.commit_editor_transaction(&transaction, Some(plan))
    }
}

impl App {
    pub(in crate::app) fn focus_surface(&mut self, direction: SpatialDirection) {
        self.reconcile_editor_windows();
        let Some(target) = self.window_session.spatial_target(direction) else {
            return;
        };
        let result = self
            .window_session
            .plan_focus(
                target,
                &self.state.window_version(),
                self.extension.contributions().version(),
            )
            .and_then(|plan| self.apply_window_plan(plan));
        if let Err(error) = result {
            self.show_error(error);
            return;
        }
        self.cancel_pending_mapping();
        self.input.cancel_pending();
        self.ensure_cursor_visible();
        self.sync_terminal_panel();
    }
}

impl App {
    pub(in crate::app) fn apply_layout_action(&mut self, action: LayoutAction) {
        self.reconcile_editor_windows();
        if matches!(action, LayoutAction::ClosePane | LayoutAction::CloseTab) {
            match self
                .window_session
                .begin_layout_close(action, &(&*self.state).into())
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
            return;
        }
        let plan = match self.window_session.plan_layout(
            action,
            &(&*self.state).into(),
            self.extension.contributions().version(),
        ) {
            Ok(plan) => plan,
            Err(crate::app::windows::LayoutPlanError::Dirty) => {
                self.show_error(self.message("dirty_close", &[]));
                return;
            }
            Err(crate::app::windows::LayoutPlanError::Rejected(error)) => {
                self.show_error(error);
                return;
            }
        };
        if let Err(error) = self.apply_window_plan(plan) {
            self.show_error(error);
        } else {
            self.cancel_pending_mapping();
            self.input.cancel_pending();
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    pub(in crate::app) fn handle_editor_close_step(&mut self, step: EditorCloseStep) {
        match step {
            EditorCloseStep::Prompt { title, message } => self.open_confirmation_actions(
                PromptKind::DirtyClose,
                title,
                message,
                crate::ui::ConfirmationChoice::No,
                "Save",
                "Discard",
            ),
            EditorCloseStep::Save(token) => {
                let buffer = token.buffer();
                let retained = token.clone();
                match self.request_save_buffer(buffer, false, None, false, Some(token)) {
                    Ok(_) => self.notify_message(self.message("saving", &[]), false),
                    Err(error) => {
                        self.window_session.fail_editor_close_save(&retained);
                        self.show_document_failure(error);
                    }
                }
            }
            EditorCloseStep::Ready { target, discarded } => {
                self.apply_accepted_editor_close(target, discarded);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn submit_dirty_close(&mut self, selected: crate::ui::ConfirmationChoice) {
        let step = self.window_session.choose_editor_close(
            selected == crate::ui::ConfirmationChoice::Yes,
            &(&*self.state).into(),
        );
        self.close_prompt_with_editor_close(false);
        match step {
            Ok(step) => self.handle_editor_close_step(step),
            Err(error) => {
                self.window_session.cancel_editor_close();
                self.show_error(error);
            }
        }
    }
}

impl App {
    fn apply_accepted_editor_close(
        &mut self,
        target: EditorCloseTarget,
        discarded: Vec<crate::editor::BufferId>,
    ) {
        match target {
            EditorCloseTarget::Buffer { view, buffer } => {
                self.close_buffer_target(view, buffer);
            }
            EditorCloseTarget::Layout(_) | EditorCloseTarget::Window { .. } => {
                let closed = discarded
                    .iter()
                    .filter_map(|id| {
                        self.state
                            .buffers
                            .get(*id)
                            .map(|buffer| (*id, buffer.path.clone(), !buffer.dirty))
                    })
                    .collect::<Vec<_>>();
                let plan = self.window_session.plan_accepted_editor_close(
                    target,
                    discarded,
                    &(&*self.state).into(),
                    self.extension.contributions().version(),
                );
                match plan {
                    Ok(plan) => {
                        if let Err(error) = self.apply_window_plan(plan) {
                            self.show_error(error);
                        } else {
                            for (id, path, clean) in closed {
                                self.finish_buffer_close(id, path.as_deref(), clean);
                            }
                            self.cancel_pending_mapping();
                            self.input.cancel_pending();
                            self.ensure_cursor_visible();
                        }
                    }
                    Err(LayoutPlanError::Dirty) => unreachable!("accepted close bypasses review"),
                    Err(LayoutPlanError::Rejected(error)) => self.show_error(error),
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn set_explorer_focus(&mut self, focused: bool) {
        if focused {
            self.window_session.focus_explorer();
        } else {
            self.window_session.leave_explorer();
        }
        self.project_window_activation();
    }
}

impl App {
    pub(in crate::app) fn project_window_activation(&mut self) {
        if let Some(id) = self
            .window_session
            .focused_terminal()
            .map(PtyId)
            .filter(|id| self.terminal_session.contains(*id))
        {
            self.terminal_session.display(id);
        }
        self.ui.dashboard = self.window_session.dashboard_visible();
        self.ui.explorer_focused = self.window_session.explorer_focused();
        if let Some(view) = self.window_session.focused_explorer() {
            let _ = self
                .explorer
                .activate_view(crate::app::explorer::ExplorerViewId(view));
        }
    }
}

impl App {
    pub(in crate::app) fn project_window_content_tabs(&mut self) {
        self.sync_ui_diagnostics();
        self.window_session.project_content_tabs(
            &(&*self.state).into(),
            self.diagnostics.counts(),
            self.configuration.settings(),
            self.configuration.reload_id(),
        );
        self.ui.content_tabs = self.window_session.content_tabs().to_vec();
        self.ui.chrome = self.window_session.chrome_layout().clone();
    }
}

impl App {
    pub(in crate::app) fn project_window_geometry(&mut self) {
        self.project_window_activation();
        self.ui.compositor_layout = self.window_session.layout().cloned();
        self.refresh_window_content();
        self.follow_agent_pane();
    }
}

impl App {
    pub(in crate::app) fn refresh_window_content(&mut self) {
        self.sync_ui_diagnostics();
        self.close_stale_picker();
        self.cancel_stale_explorer_intents();
        self.window_session
            .project_content(self.ui.picker.as_ref(), self.configuration.settings());
        if let Some(geometry) = self.window_session.content().picker {
            if let Some(scroll) = self.search.picker_scroll(usize::from(geometry.list.height))
                && let Some(ui) = &mut self.ui.picker
            {
                ui.scroll = scroll;
            }
            let preview_rows = usize::from(geometry.preview_rows);
            if let Some(scroll) = self.search.picker_preview_scroll(preview_rows)
                && let Some(ui) = &mut self.ui.picker
            {
                ui.preview_side_scroll = (scroll, scroll);
            }
        }
        self.ui.surface_content = self.window_session.content().clone();
        self.project_input_documentation();
    }
}

impl App {
    fn sync_plugin_window_ui(&mut self) {
        let projection = self.compositor_projection().cloned();
        self.extension.synchronize_windows(
            projection.as_ref(),
            self.configuration.settings().editor.ambiguous_width
                == crate::config::AmbiguousWidth::Wide,
        );
        self.ui.plugin_surfaces = self.extension.window_snapshots();
        self.ui.private_surfaces = self
            .extension
            .private_snapshots(self.window_session.graph());
    }
}

impl App {
    pub(in crate::app) fn compositor_projection(&self) -> Option<&CompositorProjection> {
        self.window_session.current_for_editor(
            &self.state.window_version(),
            (self.window_session.width(), self.window_session.height()),
        )
    }
}

impl App {
    pub(in crate::app) fn workspace_bounds(&self) -> Option<WindowRect> {
        self.compositor_projection()
            .map(|projection| projection.workspace_bounds)
    }
}

impl App {
    pub(in crate::app) fn explorer_window_rect(&self) -> Option<WindowRect> {
        let layout = self.window_session.layout()?;
        let window = layout.surfaces.explorer?;
        self.compositor_projection()?
            .window(window)
            .map(|window| window.rect)
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn workspace_render_height(&self) -> u16 {
        self.workspace_bounds()
            .map_or(self.window_session.height(), |bounds| bounds.height)
    }
}

impl App {
    fn window_target_error(&self, error: crate::app::windows::TargetError) -> String {
        match error {
            crate::app::windows::TargetError::MissingTerminal => {
                self.message("terminal_missing", &[])
            }
            crate::app::windows::TargetError::Rejected(error) => error,
        }
    }
}

impl App {
    pub(in crate::app) fn apply_native_window_placement(
        &mut self,
        target: NativeSurfaceTarget,
        placement: WindowPlacement,
    ) -> Result<(), String> {
        // Commands operate only against the exact application projection that
        // was accepted for the current editor source revision.
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        self.window_session
            .place_native(target, placement, &mut self.state)?;
        self.reconcile_editor_windows();
        if matches!(target, NativeSurfaceTarget::Terminal(_))
            && placement != WindowPlacement::Hidden
        {
            self.resize_active_terminal_to_panel();
        }
        self.sync_terminal_panel();
        self.ensure_cursor_visible();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn show_native_window(
        &mut self,
        target: NativeSurfaceTarget,
    ) -> Result<(), String> {
        if self.compositor_projection().is_none() {
            return Err("universal compositor projection is missing or stale".into());
        }
        let explorer_fallback = WindowPlacement::Docked {
            edge: self.explorer_edge(),
            size: explorer_panel_width(self.configuration.settings(), self.window_session.width())
                .unwrap_or(1)
                .saturating_add(1),
        };
        let terminal_fallback = WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: self.window_session.terminal_height().max(1),
        };
        self.window_session.show_native(
            target,
            explorer_fallback,
            terminal_fallback,
            &mut self.state,
        )?;
        self.reconcile_editor_windows();
        if matches!(target, NativeSurfaceTarget::Terminal(_)) {
            self.resize_active_terminal_to_panel();
        }
        self.sync_terminal_panel();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn current_native_placement(
        &self,
        target: NativeSurfaceTarget,
    ) -> Result<WindowPlacement, String> {
        self.window_session.native_placement(target)
    }
}

impl App {
    pub(in crate::app) fn clone_graph_window(
        &mut self,
        target: &str,
        axis: crate::windows::TileAxis,
    ) -> Result<(), String> {
        let selected_terminal = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected_terminal)
            .map_err(|error| self.window_target_error(error))?;
        let explorer_source = match self.window_session.native_ids().target_for_window(window) {
            Some(NativeSurfaceTarget::Explorer) => Some(1),
            Some(NativeSurfaceTarget::ExplorerView(view)) => Some(view),
            _ => None,
        };
        let Some(source) = explorer_source else {
            return self.run_window_command(WindowCommand::Clone(target, axis, PANE_DEFAULT_RATIO));
        };
        let explorer = self
            .explorer
            .prepare_clone(crate::app::explorer::ExplorerViewId(source))?;
        self.explorer.check_clone(&explorer)?;
        let plan = self
            .window_session
            .plan_window_command(
                WindowCommand::CloneExplorer(target, source, explorer.id().0, axis),
                &(&*self.state).into(),
                CommandPolicy {
                    selected_terminal,
                    pane_step: PANE_RESIZE_STEP,
                    terminal_step: TERMINAL_PANEL_RESIZE_STEP,
                },
                self.extension.contributions().version(),
            )
            .map_err(|error| match error {
                CommandError::Dirty => self.message("dirty_close", &[]),
                CommandError::Target(error) => self.window_target_error(error),
                CommandError::Rejected(error) => error,
            })?;
        self.explorer.check_clone(&explorer)?;
        match self.window_session.accept_window_command(
            plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
            &mut self.state,
        )? {
            CommandEffect::ClonedExplorer(view) if view == explorer.id().0 => {}
            _ => return Err("explorer clone returned the wrong window effect".into()),
        }
        self.explorer.accept_clone(explorer)?;
        self.project_window_activation();
        self.reconcile_editor_windows();
        self.sync_explorer_ui();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn place_graph_window(
        &mut self,
        target: &str,
        placement: WindowPlacement,
    ) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            self.apply_native_window_placement(native, placement)
        } else {
            self.run_window_command(WindowCommand::Place(target, placement))
        }
    }
}

impl App {
    pub(in crate::app) fn show_graph_window(&mut self, target: &str) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            self.show_native_window(native)
        } else {
            self.run_window_command(WindowCommand::Show(target))
        }
    }
}

impl App {
    pub(in crate::app) fn resize_graph_window(
        &mut self,
        target: &str,
        amount: i16,
    ) -> Result<(), String> {
        self.run_window_command(WindowCommand::Resize(target, amount))
    }
}

impl App {
    pub(in crate::app) fn resize_focused_window(
        &mut self,
        direction: SpatialDirection,
        count: u64,
    ) -> Result<(), String> {
        self.reconcile_editor_windows();
        self.run_window_command(WindowCommand::Direction(direction, None, count))
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn resize_native_window_direction(
        &mut self,
        target: NativeSurfaceTarget,
        direction: SpatialDirection,
    ) -> Result<(), String> {
        self.run_window_command(WindowCommand::Direction(direction, Some(target), 1))
    }
}

impl App {
    pub(in crate::app) fn close_graph_window(&mut self, target: &str) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if self.state.window_view(window).map(|_| window).is_some() {
            self.reconcile_editor_windows();
            let window = self
                .window_session
                .resolve_graph_target(target, selected)
                .map_err(|error| self.window_target_error(error))?;
            let step = self
                .window_session
                .begin_window_close(window, &(&*self.state).into())?;
            self.handle_editor_close_step(step);
            Ok(())
        } else {
            self.run_window_command(WindowCommand::Close(target))
        }
    }
}

impl App {
    pub(in crate::app) fn move_graph_window(
        &mut self,
        target: &str,
        x: u16,
        y: u16,
    ) -> Result<(), String> {
        let selected = self.terminal_session.selected().map(|id| id.0);
        let window = self
            .window_session
            .resolve_graph_target(target, selected)
            .map_err(|error| self.window_target_error(error))?;
        if let Some(native) = self.window_session.native_ids().target_for_window(window) {
            let WindowPlacement::Floating(mut rect) = self.current_native_placement(native)? else {
                return Err("only floating ordinary windows can be moved".into());
            };
            rect.x = x;
            rect.y = y;
            self.apply_native_window_placement(native, WindowPlacement::Floating(rect))
        } else {
            self.run_window_command(WindowCommand::Move(target, x, y))
        }
    }
}

impl App {
    pub(in crate::app) fn swap_graph_windows(
        &mut self,
        first: &str,
        second: &str,
    ) -> Result<(), String> {
        self.run_window_command(WindowCommand::Swap(first, second))
    }
}

impl App {
    fn run_window_command(&mut self, command: WindowCommand<'_>) -> Result<(), String> {
        let plan = self
            .window_session
            .plan_window_command(
                command,
                &(&*self.state).into(),
                CommandPolicy {
                    // The terminal with the keys, as the one ctrl-h/l moved to need not be selected.
                    selected_terminal: self
                        .window_session
                        .focused_terminal()
                        .or(self.terminal_session.selected().map(|id| id.0)),
                    pane_step: PANE_RESIZE_STEP,
                    terminal_step: TERMINAL_PANEL_RESIZE_STEP,
                },
                self.extension.contributions().version(),
            )
            .map_err(|error| match error {
                CommandError::Dirty => self.message("dirty_close", &[]),
                CommandError::Target(error) => self.window_target_error(error),
                CommandError::Rejected(error) => error,
            })?;
        if !plan.operations().is_empty() {
            return self.apply_window_plan(plan);
        }
        match self.window_session.accept_window_command(
            plan,
            &self.state.window_version(),
            &self.extension.contributions().version(),
            &mut self.state,
        )? {
            CommandEffect::None => {}
            CommandEffect::ClonedExplorer(_) => {
                return Err("explorer clone bypassed its source owner".into());
            }
            CommandEffect::ClosedExplorer => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == crate::ui::PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.ui.explorer_views.clear();
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
            }
            CommandEffect::ClosedExplorerView(view) => {
                self.explorer
                    .remove_view(crate::app::explorer::ExplorerViewId(view))?;
                self.ui
                    .explorer_views
                    .retain(|candidate| candidate.view != view);
            }
            CommandEffect::ClosedPicker(kind) => {
                if self.ui.picker.as_ref().is_some_and(|p| p.surface == kind) {
                    self.close_picker();
                }
            }
            CommandEffect::CloseTerminal(id) => self.close_terminal(PtyId(id)),
        }
        self.reconcile_editor_windows();
        self.resize_active_terminal_to_panel();
        self.sync_terminal_panel();
        self.ensure_cursor_visible();
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn native_surface_signature(&self) -> NativeSurfaceSignature {
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
                    self.window_session.height(),
                    self.window_session.terminal_height(),
                )
            })
            .map_or(1, |geometry| geometry.height);
        let picker_rect = self.ui.picker.as_ref().and_then(|picker| {
            crate::ui::render::picker::picker_window_geometry(
                picker,
                WindowRect {
                    x: 0,
                    y: 0,
                    width: self.window_session.width(),
                    height: self.window_session.height(),
                },
            )
        });
        NativeSurfaceSignature {
            editor_tab: self.state.active_tab(),
            editor_window: self.state.focused_window(),
            focused_plugin: match self.window_session.focus() {
                crate::app::windows::FocusTarget::Plugin(window) => Some(window),
                _ => None,
            },
            explorer_visible: self.ui.explorer.is_some(),
            explorer_views: self
                .explorer
                .view_ids()
                .map(|view| view.0)
                .collect::<Vec<_>>()
                .into(),
            focused_explorer: self
                .ui
                .explorer
                .as_ref()
                .and_then(|_| self.window_session.focused_explorer()),
            explorer_size,
            explorer_edge: self.explorer_edge(),
            active_terminal: self.displayed_terminal().map(|id| id.0),
            shown_terminals: self
                .terminal_session
                .displayed()
                .into_iter()
                .chain(self.terminal_session.side_terminals())
                .map(|id| id.0)
                .collect::<Vec<_>>()
                .into(),
            terminal_focused: self.displayed_terminal().is_some_and(|id| {
                self.window_session.focused_terminal() == Some(id.0)
                    && self.window_session.native_ids().terminal_placement(id.0)
                        != Some(WindowPlacement::Hidden)
            }),
            terminal_size,
            terminal_shrunk: self.window_session.terminal_shrunk(),
            terminal_set_revision: self.terminal_session.revision(),
            terminal_width: self.window_session.width(),
            terminal_height: self.window_session.height(),
            dashboard_visible: self.window_session.dashboard_visible()
                || self.state.active_view().is_none(),
            picker_rect,
            picker_surface: self
                .ui
                .picker
                .as_ref()
                .filter(|_| picker_rect.is_some())
                .map(|picker| picker.surface),
            picker_title: self
                .ui
                .picker
                .as_ref()
                .map_or("Picker", |picker| &picker.title)
                .to_owned(),
            protected_prompt_height: self.input.prompt().map_or(0, |prompt| {
                let (width, height) = crate::ui::prompt_surface_dimensions(
                    &self.state,
                    &self.ui,
                    self.window_session.width(),
                    self.window_session.height(),
                );
                crate::ui::prompt_geometry_for_state(
                    width,
                    height,
                    self.configuration.settings(),
                    prompt,
                )
                .height
            }),
            configuration_reload_id: self.configuration.reload_id(),
            screen_bottom: screen_foot_rows(
                &self.state,
                &self.ui,
                self.configuration.settings(),
                self.window_session.height(),
            ),
        }
    }
}

impl App {
    pub(in crate::app) fn reconcile_editor_windows(&mut self) {
        let scratch_label = &SCRATCH_LABEL;
        let editor_current = self.window_session.core.source.as_ref()
            == Some(&self.state.window_version())
            && self.window_session.core.scratch_label.as_deref() == Some(scratch_label);
        if !editor_current && let Err(error) = self.state.register_window_content() {
            self.health.record(
                LogLevel::Error,
                "windows",
                "window-content-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            );
            return;
        }
        let signature = self.native_surface_signature();
        if editor_current
            && self.window_session.native_ids().is_current(&signature)
            && self.window_session.graph().revision() == self.ui.window_graph_revision
            && self
                .window_session
                .content_current(self.configuration.reload_id())
            && self.compositor_projection().is_some()
        {
            return;
        }
        let result = if editor_current && self.window_session.native_ids().is_current(&signature) {
            Ok(())
        } else {
            let terminals = self.terminal_session.descriptions();
            self.window_session
                .prepare_reconcile(
                    &(&*self.state).into(),
                    scratch_label,
                    NativeDescriptions {
                        signature: self.native_surface_signature(),
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
                    self.window_session.accept_current(
                        candidate,
                        &self.state.window_version(),
                        &mut self.state,
                    )
                })
        };
        match result {
            Ok(()) => {
                self.project_window_content_tabs();
                let graph = self.window_session.graph();
                let surfaces = self.window_session.native_ids();
                self.ui.editor_window_layout = self.state.window_revision().and_then(|revision| {
                    let tab = self.state.active_tab()?;
                    Some(EditorWindowLayoutState {
                        source_revision: revision,
                        layout: crate::windows::TiledLayoutSnapshot {
                            root: self.state.tab_layout(tab)?,
                            focused: graph.focused(),
                        },
                        bindings: self
                            .state
                            .tab_windows(tab)
                            .into_iter()
                            .filter_map(|window| {
                                Some(UiEditorWindowBinding {
                                    window,
                                    view: self.state.window_view(window)?,
                                })
                            })
                            .collect(),
                    })
                });
                self.ui.window_graph_revision = graph.revision();
                let projection = self.state.window_revision().and_then(|revision| {
                    let graph_revision = graph.revision()?;
                    let tab = self
                        .state
                        .active_tab()
                        .or_else(|| surfaces.workspace_tab())?;
                    let snapshot = graph.compositor_snapshot(tab)?;
                    let screen_bottom = screen_foot_rows(
                        &self.state,
                        &self.ui,
                        self.configuration.settings(),
                        self.window_session.height(),
                    );
                    let preliminary = snapshot.project_with_tiled_insets(
                        WindowRect {
                            x: 0,
                            y: 0,
                            width: self.window_session.width(),
                            height: self.window_session.height(),
                        },
                        PANE_MINIMUM_WIDTH,
                        PANE_MINIMUM_HEIGHT,
                        crate::windows::TiledInsets {
                            screen_bottom,
                            ..Default::default()
                        },
                    );
                    let top =
                        workspace_top_rows(&self.state, &self.ui, self.configuration.settings());
                    let (left, right, bottom_panes) = self
                        .ui
                        .debug_panes
                        .as_ref()
                        .filter(|_| !self.ui.dashboard && self.state.active_view().is_some())
                        .and_then(|_| {
                            crate::ui::debug_pane_insets(
                                preliminary.workspace_bounds.width,
                                preliminary.workspace_bounds.height.saturating_sub(top),
                            )
                        })
                        .unwrap_or_default();
                    let protected = self.input.prompt().map(|prompt| {
                        let prompt_height = preliminary.workspace_bounds.height.saturating_sub(top);
                        let geometry = crate::ui::prompt_geometry_for_state(
                            self.window_session.width(),
                            prompt_height,
                            self.configuration.settings(),
                            prompt,
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
                        minimum_size: (PANE_MINIMUM_WIDTH, PANE_MINIMUM_HEIGHT),
                        insets: crate::windows::TiledInsets {
                            screen_bottom,
                            top,
                            bottom: bottom_panes,
                            left,
                            right,
                        },
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
            Err(error) => self.health.record(
                LogLevel::Error,
                "windows",
                "window-reconciliation-rejected",
                BTreeMap::from([("error".into(), error.to_string())]),
            ),
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
