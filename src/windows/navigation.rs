//! Logical window order and split navigation, including temporarily hidden tiles.

use crate::windows::tree;
use crate::windows::{
    SpatialDirection, TileAxis, TiledLayoutNode, WindowError, WindowGraph, WindowId,
    WorkspaceTabDescriptor, WorkspaceTabId,
};

impl WindowGraph {
    pub fn clear(&mut self) -> Result<(), WindowError> {
        self.checked_mutation(|graph| {
            graph.windows.clear();
            graph.views.clear();
            graph.contents.clear();
            graph.workspace_tabs.clear();
            graph.remembered_focus.clear();
            graph.owner_focus.clear();
            graph.active_tab = None;
            graph.focused = None;
            Ok(())
        })
    }

    pub fn check_window_limit(&self, owner: &str) -> Result<(), WindowError> {
        if self
            .windows
            .values()
            .filter(|window| window.owner == owner)
            .count()
            >= crate::windows::MAX_WINDOWS
        {
            Err(WindowError::TooManyTiledWindows)
        } else {
            Ok(())
        }
    }
    pub fn workspace_tabs(&self) -> impl Iterator<Item = &WorkspaceTabDescriptor> {
        self.workspace_tabs.values()
    }

    pub fn logical_layout(&self, tab: WorkspaceTabId) -> Option<&TiledLayoutNode> {
        self.workspace_tabs.get(&tab)?.tiled_root.as_ref()
    }

    pub fn last_focused(&self, tab: WorkspaceTabId, owner: &str) -> Option<WindowId> {
        self.owner_focus
            .get(&(tab, owner.to_owned()))
            .copied()
            .or_else(|| {
                self.ordered_windows(tab).into_iter().find(|id| {
                    self.windows
                        .get(id)
                        .is_some_and(|window| window.owner == owner)
                })
            })
    }

    pub fn ordered_windows(&self, tab: WorkspaceTabId) -> Vec<WindowId> {
        let mut ids = self
            .logical_layout(tab)
            .map_or_else(Vec::new, TiledLayoutNode::windows);
        for window in self.windows.values().filter(|window| window.tab == tab) {
            if !ids.contains(&window.id) {
                ids.push(window.id);
            }
        }
        ids
    }

    // A hidden document remains selectable for close and save operations without taking
    // keyboard focus from a visible native surface.
    pub fn select_window(&mut self, id: WindowId) -> Result<(), WindowError> {
        self.checked_mutation(|graph| {
            let window = graph.windows.get(&id).ok_or(WindowError::UnknownWindow)?;
            graph
                .owner_focus
                .insert((window.tab, window.owner.clone()), id);
            graph.active_tab = Some(window.tab);
            if graph.can_focus(id) {
                graph.focused = Some(id);
            } else if graph
                .focused
                .and_then(|focused| graph.windows.get(&focused))
                .is_some_and(|focused| Some(focused.tab) != graph.active_tab)
            {
                graph.focused = None;
                graph.select_focus_fallback();
            }
            Ok(())
        })
    }

    pub fn show_window(&mut self, id: WindowId) -> Result<(), WindowError> {
        let placement = self
            .window(id)
            .ok_or(WindowError::UnknownWindow)?
            .restore_placement;
        self.place(id, placement)
    }

    pub fn split_windows(&self, first: WindowId, second: WindowId) -> Option<Vec<WindowId>> {
        let tab = self.window(first)?.tab;
        let mut root = self.logical_layout(tab)?.clone();
        tree::split_between(&mut root, first, second).map(|node| node.windows())
    }

    pub fn set_split_ratio(
        &mut self,
        first: WindowId,
        second: WindowId,
        ratio: u16,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|graph| {
            let tab = graph.window(first).ok_or(WindowError::UnknownWindow)?.tab;
            let root = graph
                .workspace_tabs
                .get_mut(&tab)
                .and_then(|tab| tab.tiled_root.as_mut())
                .ok_or(WindowError::MissingTiledLayout)?;
            let Some(TiledLayoutNode::Split { ratio: current, .. }) =
                tree::split_between(root, first, second)
            else {
                return Err(WindowError::WindowNotInTiledLayout);
            };
            *current = ratio.clamp(100, 900);
            Ok(())
        })
    }

    pub fn resize_window_split(
        &mut self,
        id: WindowId,
        axis: Option<TileAxis>,
        delta: i16,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|graph| {
            let tab = graph.window(id).ok_or(WindowError::UnknownWindow)?.tab;
            let root = graph
                .workspace_tabs
                .get_mut(&tab)
                .and_then(|tab| tab.tiled_root.as_mut())
                .ok_or(WindowError::MissingTiledLayout)?;
            let changed = tree::resize_parent(root, id, axis, delta, 100, 900);
            if changed {
                Ok(())
            } else {
                Err(WindowError::WindowNotInTiledLayout)
            }
        })
    }

    pub fn directional_window(
        &self,
        id: WindowId,
        direction: SpatialDirection,
        owner: &str,
    ) -> Option<WindowId> {
        let tab = self.window(id)?.tab;
        let root = self.logical_layout(tab)?;
        let mut rectangles = Vec::new();
        tree::collect_focus_rectangles(
            root,
            tree::FocusRectangle {
                x: 0,
                y: 0,
                width: 1_000_000,
                height: 1_000_000,
            },
            &mut rectangles,
        );
        let source = rectangles.iter().find(|(window, _)| *window == id)?.1;
        rectangles
            .into_iter()
            .filter(|(window, _)| {
                *window != id
                    && self
                        .window(*window)
                        .is_some_and(|window| window.owner == owner)
            })
            .filter_map(|(window, rect)| {
                tree::focus_score(source, rect, direction).map(|score| (score, window))
            })
            .min_by_key(|(score, id)| (*score, id.0))
            .map(|(_, window)| window)
    }

    pub fn resized_split(
        &self,
        window: WindowId,
        axis: TileAxis,
        delta: i16,
    ) -> Option<(WindowId, WindowId, u16)> {
        fn changed(
            before: &TiledLayoutNode,
            after: &TiledLayoutNode,
        ) -> Option<(WindowId, WindowId, u16)> {
            let (
                TiledLayoutNode::Split {
                    ratio: old,
                    first: left,
                    second: right,
                    ..
                },
                TiledLayoutNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                },
            ) = (before, after)
            else {
                return None;
            };
            if old != ratio {
                return Some((first.windows()[0], second.windows()[0], *ratio));
            }
            changed(left, first).or_else(|| changed(right, second))
        }
        let tab = self.window(window)?.tab;
        let before = self.tiled_layout_snapshot(tab)?.root;
        let mut after = before.clone();
        if !tree::resize_parent(&mut after, window, Some(axis), delta, 100, 900) {
            return None;
        }
        changed(&before, &after).or_else(|| {
            // A clamped resize is still an applicable split, not a request to resize a dock.
            fn split(
                node: &TiledLayoutNode,
                window: WindowId,
                axis: TileAxis,
            ) -> Option<(WindowId, WindowId, u16)> {
                let TiledLayoutNode::Split {
                    axis: current,
                    ratio,
                    first,
                    second,
                } = node
                else {
                    return None;
                };
                let child = if first.windows().contains(&window) {
                    first
                } else if second.windows().contains(&window) {
                    second
                } else {
                    return None;
                };
                split(child, window, axis).or_else(|| {
                    (*current == axis).then(|| (first.windows()[0], second.windows()[0], *ratio))
                })
            }
            split(&after, window, axis)
        })
    }

    pub fn replace_layout(
        &mut self,
        tab: WorkspaceTabId,
        root: Option<TiledLayoutNode>,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|graph| {
            graph
                .workspace_tabs
                .get_mut(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?
                .tiled_root = root;
            Ok(())
        })
    }
}
