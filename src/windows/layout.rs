//! Tiled layouts, window placement and compositor snapshots.

use crate::windows::MAXIMUM_PROTECTED_REGIONS;
use crate::windows::error::WindowError;
use crate::windows::identity::WindowId;
use crate::windows::identity::WorkspaceTabId;
use crate::windows::projection::intersect_rect;
use crate::windows::projection::project_dock;
use crate::windows::projection::project_float;
use crate::windows::projection::project_tiled_node;
use crate::windows::projection::push_compositor_window;
use crate::windows::projection::rect_contains;
use crate::windows::projection::spatial_score;
use crate::windows::tree::contains_window;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentAdapterKind {
    TextBuffer,
    TerminalSession,
    VirtualDocument,
    DeclarativeUi,
    PrivateCellSurface,
    ExternalTui,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DockEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowPlacement {
    Tiled,
    Docked { edge: DockEdge, size: u16 },
    Floating(WindowRect),
    Fullscreen,
    Overlay(WindowRect),
    Hidden,
}

impl WindowPlacement {
    pub(crate) fn validate(self) -> Result<Self, WindowError> {
        let valid = match self {
            Self::Tiled => true,
            Self::Docked { size, .. } => size > 0,
            Self::Floating(rect) | Self::Overlay(rect) => rect.width > 0 && rect.height > 0,
            Self::Fullscreen | Self::Hidden => true,
        };
        valid.then_some(self).ok_or(WindowError::InvalidPlacement)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TiledLayoutNode {
    Window(WindowId),
    Split {
        axis: TileAxis,
        ratio: u16,
        first: Box<TiledLayoutNode>,
        second: Box<TiledLayoutNode>,
    },
}

impl TiledLayoutNode {
    pub fn windows(&self) -> Vec<WindowId> {
        crate::windows::tree::window_ids(self)
    }

    pub fn filtered(&self, keep: &impl Fn(WindowId) -> bool) -> Option<Self> {
        match self {
            Self::Window(window) => keep(*window).then_some(self.clone()),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.filtered(keep), second.filtered(keep)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedBorder {
    pub x: u16,
    pub y: u16,
    pub length: u16,
    pub axis: TileAxis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedWindow {
    pub window: WindowId,
    pub rect: WindowRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TiledLayoutProjection {
    pub windows: Vec<ProjectedWindow>,
    pub borders: Vec<ProjectedBorder>,
    pub collapsed_to_focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TiledLayoutSnapshot {
    pub root: TiledLayoutNode,
    pub focused: Option<WindowId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositorLayer {
    Tiled,
    Docked,
    Floating,
    Fullscreen,
    Overlay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompositorWindowSnapshot {
    pub window: WindowId,
    pub placement: WindowPlacement,
    pub adapter: ContentAdapterKind,
    pub focusable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositorSnapshot {
    pub tab: WorkspaceTabId,
    pub tiled: Option<TiledLayoutSnapshot>,
    pub windows: Vec<CompositorWindowSnapshot>,
    pub focused: Option<WindowId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedCompositorWindow {
    pub window: WindowId,
    pub rect: WindowRect,
    pub layer: CompositorLayer,
    pub z_index: u16,
    pub focused: bool,
    pub focusable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialDirection {
    Left,
    Down,
    Up,
    Right,
}

/// Rows and columns the tiled windows leave to the editor's own chrome and the debug panes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TiledInsets {
    /// Rows at the foot of the screen that docks and tiled windows both stay above.
    pub screen_bottom: u16,
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositorProjection {
    pub bounds: WindowRect,
    /// Remaining editor workspace after generic docks, before editor chrome.
    pub workspace_bounds: WindowRect,
    /// Exact tiled content bounds after host-owned editor chrome insets.
    pub tiled_bounds: WindowRect,
    pub windows: Vec<ProjectedCompositorWindow>,
    pub borders: Vec<ProjectedBorder>,
    /// Host-owned prompt/crash/permission rectangles above ordinary windows.
    pub protected_regions: Vec<WindowRect>,
    pub collapsed_to_focused: bool,
}

impl CompositorProjection {
    #[must_use]
    pub fn window(&self, window: WindowId) -> Option<ProjectedCompositorWindow> {
        self.windows
            .iter()
            .copied()
            .find(|candidate| candidate.window == window)
    }

    #[must_use]
    pub fn hit_test(&self, column: u16, row: u16) -> Option<WindowId> {
        if self
            .protected_regions
            .iter()
            .any(|rect| rect_contains(*rect, column, row))
        {
            return None;
        }
        self.windows
            .iter()
            .rev()
            .find(|candidate| rect_contains(candidate.rect, column, row))
            .map(|candidate| candidate.window)
    }

    pub fn set_protected_regions(&mut self, regions: impl IntoIterator<Item = WindowRect>) {
        self.protected_regions = regions
            .into_iter()
            .take(MAXIMUM_PROTECTED_REGIONS)
            .filter_map(|region| intersect_rect(region, self.bounds))
            .collect();
    }

    #[must_use]
    pub fn spatial_neighbor(
        &self,
        source: WindowId,
        direction: SpatialDirection,
    ) -> Option<WindowId> {
        let source_rect = self.window(source)?.rect;
        self.windows
            .iter()
            .filter(|candidate| candidate.focusable && candidate.window != source)
            .filter_map(|candidate| {
                spatial_score(source_rect, candidate.rect, direction)
                    .map(|score| (score, candidate.z_index, candidate.window))
            })
            .min_by_key(|(score, z, id)| (*score, std::cmp::Reverse(*z), id.0))
            .map(|(_, _, window)| window)
    }
}

impl CompositorSnapshot {
    #[must_use]
    pub fn project(
        &self,
        bounds: WindowRect,
        minimum_tiled_width: u16,
        minimum_tiled_height: u16,
    ) -> CompositorProjection {
        self.project_with_tiled_insets(
            bounds,
            minimum_tiled_width,
            minimum_tiled_height,
            TiledInsets::default(),
        )
    }

    #[must_use]
    pub fn project_with_tiled_insets(
        &self,
        bounds: WindowRect,
        minimum_tiled_width: u16,
        minimum_tiled_height: u16,
        insets: TiledInsets,
    ) -> CompositorProjection {
        let mut output = CompositorProjection {
            bounds,
            workspace_bounds: bounds,
            tiled_bounds: bounds,
            windows: Vec::new(),
            borders: Vec::new(),
            protected_regions: Vec::new(),
            collapsed_to_focused: false,
        };
        let fullscreen = self
            .windows
            .iter()
            .find(|window| window.placement == WindowPlacement::Fullscreen);
        if let Some(fullscreen) = fullscreen {
            push_compositor_window(
                &mut output,
                *fullscreen,
                bounds,
                CompositorLayer::Fullscreen,
                self.focused,
            );
        } else {
            let mut remaining = WindowRect {
                height: bounds.height.saturating_sub(insets.screen_bottom),
                ..bounds
            };
            // Horizontal docks reserve the full width first; side docks then
            // consume only the remaining vertical workspace.
            for edge in [
                DockEdge::Top,
                DockEdge::Bottom,
                DockEdge::Left,
                DockEdge::Right,
            ] {
                for window in self.windows.iter().filter(|window| {
                    matches!(window.placement, WindowPlacement::Docked { edge: candidate, .. } if candidate == edge)
                }) {
                    let WindowPlacement::Docked { size, .. } = window.placement else {
                        continue;
                    };
                    let (reserve_width, reserve_height) = if self.tiled.is_some() {
                        (
                            minimum_tiled_width.max(1).saturating_add(insets.left).saturating_add(insets.right),
                            minimum_tiled_height.max(1).saturating_add(insets.top).saturating_add(insets.bottom),
                        )
                    } else {
                        (0, 0)
                    };
                    let (rect, next) =
                        project_dock(remaining, edge, size, reserve_width, reserve_height);
                    remaining = next;
                    if rect.width > 0 && rect.height > 0 {
                        push_compositor_window(
                            &mut output,
                            *window,
                            rect,
                            CompositorLayer::Docked,
                            self.focused,
                        );
                    }
                }
            }
            output.workspace_bounds = remaining;
            let top = insets.top.min(remaining.height);
            let bottom = insets.bottom.min(remaining.height.saturating_sub(top));
            let left = insets.left.min(remaining.width);
            let right = insets.right.min(remaining.width.saturating_sub(left));
            let tiled_bounds = WindowRect {
                x: remaining.x.saturating_add(left),
                y: remaining.y.saturating_add(top),
                width: remaining.width.saturating_sub(left).saturating_sub(right),
                height: remaining.height.saturating_sub(top).saturating_sub(bottom),
            };
            output.tiled_bounds = tiled_bounds;
            if let Some(tiled) = &self.tiled {
                let projected =
                    tiled.project(tiled_bounds, minimum_tiled_width, minimum_tiled_height);
                output.collapsed_to_focused = projected.collapsed_to_focused;
                output.borders = projected.borders;
                for window in projected.windows {
                    if let Some(snapshot) = self
                        .windows
                        .iter()
                        .find(|candidate| candidate.window == window.window)
                    {
                        push_compositor_window(
                            &mut output,
                            *snapshot,
                            window.rect,
                            CompositorLayer::Tiled,
                            self.focused,
                        );
                    }
                }
            }
            for window in self
                .windows
                .iter()
                .filter(|window| matches!(window.placement, WindowPlacement::Floating(_)))
            {
                let WindowPlacement::Floating(requested) = window.placement else {
                    continue;
                };
                // Floats sit over the whole screen, docks included, as in Neovim.
                if let Some(rect) = project_float(requested, bounds) {
                    push_compositor_window(
                        &mut output,
                        *window,
                        rect,
                        CompositorLayer::Floating,
                        self.focused,
                    );
                }
            }
        }
        for window in self
            .windows
            .iter()
            .filter(|window| matches!(window.placement, WindowPlacement::Overlay(_)))
        {
            let WindowPlacement::Overlay(requested) = window.placement else {
                continue;
            };
            if let Some(rect) = intersect_rect(requested, bounds) {
                push_compositor_window(
                    &mut output,
                    *window,
                    rect,
                    CompositorLayer::Overlay,
                    self.focused,
                );
            }
        }
        output
    }
}

impl TiledLayoutSnapshot {
    #[must_use]
    pub fn project(
        &self,
        bounds: WindowRect,
        minimum_width: u16,
        minimum_height: u16,
    ) -> TiledLayoutProjection {
        let mut projection = TiledLayoutProjection {
            windows: Vec::new(),
            borders: Vec::new(),
            collapsed_to_focused: false,
        };
        project_tiled_node(&self.root, bounds, &mut projection);
        let violates_minimum = projection
            .windows
            .iter()
            .any(|window| window.rect.width < minimum_width || window.rect.height < minimum_height);
        if violates_minimum && projection.windows.len() > 1 {
            let focused = self
                .focused
                .filter(|focused| contains_window(&self.root, *focused))
                .unwrap_or_else(|| projection.windows[0].window);
            projection.windows = vec![ProjectedWindow {
                window: focused,
                rect: bounds,
            }];
            projection.borders.clear();
            projection.collapsed_to_focused = true;
        }
        projection
    }
}
