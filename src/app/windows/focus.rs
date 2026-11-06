//! Focus targets and application navigation.

use crate::app::windows::NativeSurfaceTarget;
use crate::app::windows::Windows;
use crate::windows::PANE_MINIMUM_WIDTH;
use crate::windows::SpatialDirection;
use crate::windows::WindowId;

/// The underlying keyboard target. Picker/prompt/documentation capture does not
/// erase this target; its identity is revalidated when the graph is reconciled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::app) enum FocusTarget {
    #[default]
    Editor,
    Explorer(u64),
    Terminal(u64),
    Plugin(WindowId),
}

const SHRUNK_TERMINAL_HEIGHT: u16 = 5;
const SHORT_SCREEN_HEIGHT: u16 = 30;

impl Windows {
    pub(in crate::app) fn new(dashboard: bool, terminal_height: u16) -> Self {
        Self {
            dashboard,
            terminal_height,
            ..Self::default()
        }
    }
    pub(in crate::app) fn focus(&self) -> FocusTarget {
        self.focus
    }
    pub(in crate::app) fn explorer_focused(&self) -> bool {
        matches!(self.focus(), FocusTarget::Explorer(_))
    }
    pub(in crate::app) fn focused_explorer(&self) -> Option<u64> {
        if let FocusTarget::Explorer(view) = self.focus {
            Some(view)
        } else {
            None
        }
    }
    pub(in crate::app) fn focused_terminal(&self) -> Option<u64> {
        if let FocusTarget::Terminal(id) = self.focus {
            Some(id)
        } else {
            None
        }
    }
    pub(in crate::app) fn terminal_focused(&self) -> bool {
        self.focused_terminal().is_some()
    }

    pub(in crate::app) fn dashboard_visible(&self) -> bool {
        self.dashboard
    }
    pub(in crate::app) fn open_dashboard(&mut self) {
        self.dashboard = true;
        self.invalidate_native();
    }
    pub(in crate::app) fn close_dashboard(&mut self) {
        self.dashboard = false;
        self.invalidate_native();
    }
    pub(in crate::app) fn focus_explorer(&mut self) {
        self.request_focus(FocusTarget::Explorer(1));
    }
    pub(in crate::app) fn leave_explorer(&mut self) {
        if self.explorer_focused() {
            self.request_focus(FocusTarget::Editor);
        }
    }
    pub(in crate::app) fn focus_terminal(&mut self, id: u64) {
        self.request_focus(FocusTarget::Terminal(id));
    }
    pub(in crate::app) fn leave_terminal(&mut self) {
        if self.terminal_focused() {
            self.request_focus(FocusTarget::Editor);
        }
    }
    fn request_focus(&mut self, focus: FocusTarget) {
        self.remember_before_terminal(focus);
        if self.focus != focus {
            self.focus_stamp = std::sync::Arc::new(());
            self.focus = focus;
            self.invalidate_native();
        }
    }
    fn remember_before_terminal(&mut self, focus: FocusTarget) {
        if matches!(focus, FocusTarget::Terminal(_)) && !self.terminal_focused() {
            self.before_terminal = self.focus;
        }
    }
    /// Give the keyboard back to where it was before the terminal had it.
    pub(in crate::app) fn return_from_terminal(&mut self) {
        self.request_focus(self.before_terminal);
    }
    pub(in crate::app) fn accept_graph_focus(&mut self) {
        let Some(window) = self.core.graph.focused() else {
            self.focus = FocusTarget::Editor;
            return;
        };
        match self.core.native.target_for_window(window) {
            Some(NativeSurfaceTarget::Picker(_)) => {} // temporary capture, not terminal selection
            Some(NativeSurfaceTarget::Explorer) => self.focus = FocusTarget::Explorer(1),
            Some(NativeSurfaceTarget::ExplorerView(view)) => {
                self.focus = FocusTarget::Explorer(view)
            }
            Some(NativeSurfaceTarget::Terminal(id)) => {
                self.remember_before_terminal(FocusTarget::Terminal(id));
                self.focus = FocusTarget::Terminal(id);
            }
            Some(NativeSurfaceTarget::Dashboard) => self.focus = FocusTarget::Editor,
            None => {
                self.focus = if self
                    .core
                    .graph
                    .window(window)
                    .is_some_and(|w| w.owner.starts_with("plugin."))
                {
                    FocusTarget::Plugin(window)
                } else {
                    FocusTarget::Editor
                }
            }
        }
    }
    pub(in crate::app) fn spatial_target(&self, direction: SpatialDirection) -> Option<WindowId> {
        let layout = self.layout.as_ref()?;
        let current = match self.focus {
            FocusTarget::Explorer(view) => {
                self.core.native.explorer_view(view).map(|ids| ids.window)
            }
            FocusTarget::Terminal(id) => self.core.native.terminal(id).map(|ids| ids.window),
            FocusTarget::Plugin(window) => Some(window),
            FocusTarget::Editor => self.core.graph.focused(),
        }?;
        let projection = &layout.projection;
        projection.spatial_neighbor(current, direction).or_else(|| {
            // The dashboard lies under the whole screen, so nothing is beside a dock on its
            // side; moving off a dock toward the workspace goes to the dashboard.
            let dashboard = self.core.native.dashboard().filter(|_| self.dashboard)?;
            let from = projection.window(current)?.rect;
            let workspace = projection.workspace_bounds;
            let toward = match direction {
                SpatialDirection::Left => workspace.x + workspace.width <= from.x,
                SpatialDirection::Right => from.x + from.width <= workspace.x,
                SpatialDirection::Up => workspace.y + workspace.height <= from.y,
                SpatialDirection::Down => from.y + from.height <= workspace.y,
            };
            (toward && dashboard.window != current).then_some(dashboard.window)
        })
    }
    pub(in crate::app) fn terminal_height(&self) -> u16 {
        if self.terminal_shrunk {
            self.terminal_height.min(SHRUNK_TERMINAL_HEIGHT)
        } else {
            self.terminal_height
        }
    }
    /// On a short screen a debug session takes the terminal's room for its inspector. The
    /// height the user chose comes back when the session ends.
    pub(in crate::app) fn terminal_shrunk(&self) -> bool {
        self.terminal_shrunk
    }
    pub(in crate::app) fn set_debugging(&mut self, debugging: bool) {
        self.debugging = debugging;
        self.terminal_shrunk = debugging && self.height() < SHORT_SCREEN_HEIGHT;
    }
    pub(in crate::app) fn resize_terminal(
        &mut self,
        id: u64,
        rows: i16,
        state: &mut crate::editor::EditorState,
    ) -> Result<(), String> {
        self.require_projected_graph()?;
        let target = NativeSurfaceTarget::Terminal(id);
        let current = match self.native_placement(target)? {
            crate::windows::WindowPlacement::Docked { size, .. } => size,
            crate::windows::WindowPlacement::Floating(rect) => rect.height,
            _ => self.terminal_height(),
        };
        let height = u16::try_from(
            (i32::from(current)
                + i32::from(rows) * i32::from(crate::ui::render::TERMINAL_PANEL_RESIZE_STEP))
            .clamp(
                i32::from(crate::ui::render::TERMINAL_PANEL_MINIMUM_HEIGHT),
                i32::from(crate::ui::render::TERMINAL_PANEL_MAXIMUM_HEIGHT),
            ),
        )
        .unwrap_or(current);
        let placement = match self.native_placement(target)? {
            // A terminal on the left or right, such as an agent, grows in columns.
            crate::windows::WindowPlacement::Docked {
                edge: edge @ (crate::windows::DockEdge::Left | crate::windows::DockEdge::Right),
                size,
            } => {
                let step =
                    i32::from(rows) * i32::from(crate::ui::render::TERMINAL_PANEL_RESIZE_STEP);
                let widest = i32::from(self.width().saturating_sub(PANE_MINIMUM_WIDTH)).max(20);
                let size = (i32::from(size) + step).clamp(20, widest);
                let placement = crate::windows::WindowPlacement::Docked {
                    edge,
                    size: u16::try_from(size).unwrap_or(u16::MAX),
                };
                self.place_native(target, placement, state)?;
                return Ok(());
            }
            crate::windows::WindowPlacement::Docked { edge, .. } => {
                Some(crate::windows::WindowPlacement::Docked { edge, size: height })
            }
            crate::windows::WindowPlacement::Floating(mut rect) => {
                rect.height = height;
                Some(crate::windows::WindowPlacement::Floating(rect))
            }
            _ => None,
        };
        if let Some(placement) = placement {
            self.place_native(target, placement, state)?;
        }
        self.terminal_height = height;
        self.terminal_shrunk = false;
        Ok(())
    }
    pub(in crate::app) fn constrain_terminal_height(&mut self, minimum: u16, maximum: u16) {
        self.terminal_height = self.terminal_height.clamp(minimum, maximum);
    }
}
