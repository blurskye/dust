//! Private graph, native/editor identity bridges and accepted geometry.
mod bridge;
mod content;
mod focus;
pub(in crate::editor::session) use focus::FocusTarget;
mod mutations;
mod plans;
mod plugin;
use crate::ui::window::WindowGraph;
use crate::{
    config::Config,
    ui::{
        CompositorLayoutState, NativeSurfaceWindowBinding, PickerUiState,
        geometry::{
            SurfaceContentGeometry, documentation_page_rows, explorer_body_geometry,
            picker_content_geometry,
        },
        window::{
            CompositorProjection, ContentId, ViewId, WindowId, WindowPlacement, WindowRect,
            WorkspaceTabId,
        },
    },
};
pub(in crate::editor::session) use bridge::EditorWindowBridge;
use bridge::NativeSurfaceBridge;
pub(in crate::editor::session) use bridge::{NativeSurfaceSignature, NativeSurfaceTarget};
pub(in crate::editor::session) use mutations::{NativeDescriptions, RestoreWindowPolicy};
pub use plans::LayoutAction;
pub(in crate::editor::session) use plans::{
    CommandEffect, CommandError, CommandPolicy, EditorCloseSave, EditorCloseStep,
    EditorCloseTarget, FocusLease, LayoutPlanError, NavigationStamp, TargetError, WindowCommand,
    WindowPlan,
};
pub(in crate::editor::session) use plugin::PluginWindowDraft;
use std::sync::Arc;

pub(super) struct ProjectionRequest {
    pub tab: WorkspaceTabId,
    pub source_revision: u64,
    pub graph_revision: u64,
    pub bounds: WindowRect,
    pub minimum_size: (u16, u16),
    pub insets: crate::ui::window::TiledInsets,
    pub surfaces: NativeSurfaceWindowBinding,
    pub protected: Option<WindowRect>,
}

pub(in crate::editor::session) use crate::editor::EditorTopology;

#[derive(Clone, Debug)]
pub(in crate::editor::session) struct TerminalGeometrySource {
    origin: Arc<()>,
    terminal: u64,
    content: ContentId,
    content_owner: String,
    content_generation: u64,
    view: ViewId,
    view_owner: String,
    view_generation: u64,
    window: WindowId,
    window_owner: String,
    window_generation: u64,
    tab: WorkspaceTabId,
    tab_owner: String,
    tab_generation: u64,
    placement: WindowPlacement,
}

impl PartialEq for TerminalGeometrySource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.origin, &other.origin)
            && self.terminal == other.terminal
            && self.content == other.content
            && self.content_owner == other.content_owner
            && self.content_generation == other.content_generation
            && self.view == other.view
            && self.view_owner == other.view_owner
            && self.view_generation == other.view_generation
            && self.window == other.window
            && self.window_owner == other.window_owner
            && self.window_generation == other.window_generation
            && self.tab == other.tab
            && self.tab_owner == other.tab_owner
            && self.tab_generation == other.tab_generation
            && self.placement == other.placement
    }
}
impl Eq for TerminalGeometrySource {}

#[cfg(test)]
impl TerminalGeometrySource {
    pub(in crate::editor::session) fn fixture(generation: u64) -> Self {
        Self {
            origin: Arc::new(()),
            terminal: generation,
            content: ContentId(generation),
            content_owner: "fixture".into(),
            content_generation: generation,
            view: ViewId(generation),
            view_owner: "fixture".into(),
            view_generation: generation,
            window: WindowId(generation),
            window_owner: "fixture".into(),
            window_generation: generation,
            tab: WorkspaceTabId(generation),
            tab_owner: "fixture".into(),
            tab_generation: generation,
            placement: WindowPlacement::Hidden,
        }
    }
}

#[derive(Clone, Default)]
struct WindowCore {
    graph: WindowGraph,
    editor: EditorWindowBridge,
    native: NativeSurfaceBridge,
}

pub(in crate::editor::session) struct WindowCandidate {
    origin: Arc<()>,
    epoch: u64,
    core: WindowCore,
    dashboard: Option<bool>,
}

pub(in crate::editor::session) struct EditorWindowCandidate {
    proposal: crate::editor::EditorDraft,
    catalog: crate::extensions::PluginCatalogVersion,
    candidate: WindowCandidate,
}

#[derive(Default)]
pub(super) struct WindowSession {
    origin: Arc<()>,
    epoch: u64,
    content_line: content::ContentLine,
    focus: FocusTarget,
    /// Where the keyboard was before the terminal took it, so hiding the terminal goes back there.
    before_terminal: FocusTarget,
    focus_stamp: Arc<()>,
    dashboard: bool,
    terminal_height: u16,
    terminal_shrunk: bool,
    observed_size: (u16, u16),
    core: WindowCore,
    layout: Option<CompositorLayoutState>,
    content: SurfaceContentGeometry,
    editor_close: Option<plans::EditorCloseIntent>,
}

impl WindowSession {
    pub(in crate::editor::session) fn observe_size(&mut self, size: (u16, u16)) {
        if self.observed_size != size {
            self.observed_size = size;
            self.invalidate();
        }
    }
    pub(in crate::editor::session) fn width(&self) -> u16 {
        self.observed_size.0
    }
    pub(in crate::editor::session) fn height(&self) -> u16 {
        self.observed_size.1
    }

    pub(super) fn invalidate(&mut self) {
        self.layout = None;
        self.content = SurfaceContentGeometry::default();
    }

    pub(super) fn project(&mut self, request: ProjectionRequest) {
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

    pub(in crate::editor::session) fn current_for_editor(
        &self,
        source: &crate::editor::EditorWindowVersion,
        size: (u16, u16),
    ) -> Option<&CompositorProjection> {
        if self.core.editor.source() != Some(source) {
            return None;
        }
        self.current(source.revision(), size)
    }
    pub(super) fn current(
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

    pub(super) fn layout(&self) -> Option<&CompositorLayoutState> {
        self.layout.as_ref()
    }
    pub(super) fn content(&self) -> &SurfaceContentGeometry {
        &self.content
    }

    pub(super) fn project_content(
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
                .map(|rect| explorer_body_geometry(rect)),
            terminal: layout
                .surfaces
                .terminal
                .and_then(|(_, window)| rect(window))
                .map(|rect| WindowRect {
                    x: rect.x,
                    y: rect
                        .y
                        .saturating_add(1)
                        .min(rect.y.saturating_add(rect.height)),
                    width: rect.width,
                    height: rect.height.saturating_sub(1),
                }),
            documentation_rows: documentation_page_rows(
                config,
                layout.projection.workspace_bounds.width,
                layout.projection.workspace_bounds.height,
            ),
        };
    }
}

#[cfg(test)]
mod tests;

pub(in crate::editor::session) fn rect_contains_point(
    rect: WindowRect,
    column: u16,
    row: u16,
) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub(in crate::editor::session) use plugin::{
    PluginWindowKey, PluginWindowReceipt, PluginWindowTransition,
};
