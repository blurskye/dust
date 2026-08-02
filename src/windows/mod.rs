//! Universal content, view, window, and workspace-tab ownership.
//!
//! W0 defines the validated graph. W1 reconciles editor resources into its
//! bounded tiled topology. W2 projects tiled, docked, floating, fullscreen,
//! overlay, and hidden windows together; the bundled explorer and PTYs are
//! ordinary graph resources, while editor layout mutation remains transitional.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAXIMUM_CONTENT_TYPE_ID_BYTES: usize = 128;
const MAXIMUM_OWNER_BYTES: usize = 128;
const MAXIMUM_TITLE_BYTES: usize = 4 * 1024;
const MAXIMUM_TILED_DEPTH: usize = crate::UNIVERSAL_LAYOUT_DEPTH_CEILING;
const MAXIMUM_TILED_WINDOWS: usize = crate::UNIVERSAL_LAYOUT_WINDOW_CEILING;
const MAXIMUM_NON_TILED_WINDOWS_PER_TAB: usize = 128;
const MAXIMUM_OVERLAY_WINDOWS_PER_TAB: usize = 32;
const MAXIMUM_PROTECTED_REGIONS: usize = 16;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentTypeId(String);

impl ContentTypeId {
    pub fn new(value: impl Into<String>) -> Result<Self, WindowError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAXIMUM_CONTENT_TYPE_ID_BYTES
            && value.split('.').count() >= 2
            && value.split('.').all(|segment| {
                let mut characters = segment.chars();
                characters
                    .next()
                    .is_some_and(|first| first.is_ascii_lowercase())
                    && characters.all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || character == '-'
                    })
            });
        if !valid {
            return Err(WindowError::InvalidContentType(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentId(pub u64);

/// A presentation-local state instance for one content object.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ViewId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceTabId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WindowTarget {
    Focused,
    Id(WindowId),
    ContentType(ContentTypeId),
}

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
            .min_by_key(|(score, z, id)| (*score, *z, id.0))
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
        self.project_with_tiled_insets(bounds, minimum_tiled_width, minimum_tiled_height, 0, 0)
    }

    #[must_use]
    pub fn project_with_tiled_insets(
        &self,
        bounds: WindowRect,
        minimum_tiled_width: u16,
        minimum_tiled_height: u16,
        tiled_top: u16,
        tiled_bottom: u16,
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
            let mut remaining = bounds;
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
                    let preserve_tiled = u16::from(self.tiled.is_some());
                    let (rect, next) =
                        project_dock(remaining, edge, size, preserve_tiled, preserve_tiled);
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
            let top = tiled_top.min(remaining.height);
            let bottom = tiled_bottom.min(remaining.height.saturating_sub(top));
            let tiled_bounds = WindowRect {
                y: remaining.y.saturating_add(top),
                height: remaining.height.saturating_sub(top).saturating_sub(bottom),
                ..remaining
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
                if let Some(rect) = intersect_rect(requested, bounds) {
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
                .filter(|focused| tiled_contains(&self.root, *focused))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentDescriptor {
    pub id: ContentId,
    pub content_type: ContentTypeId,
    pub adapter: ContentAdapterKind,
    pub title: String,
    pub dirty: bool,
    pub owner: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewDescriptor {
    pub id: ViewId,
    pub content: ContentId,
    pub owner: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTabDescriptor {
    pub id: WorkspaceTabId,
    pub tiled_root: Option<TiledLayoutNode>,
    pub owner: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDescriptor {
    pub id: WindowId,
    pub tab: WorkspaceTabId,
    pub view: ViewId,
    pub placement: WindowPlacement,
    pub focusable: bool,
    pub owner: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentSnapshot {
    pub id: ContentId,
    pub content_type: ContentTypeId,
    pub adapter: ContentAdapterKind,
    pub title: String,
    pub dirty: bool,
    pub active: bool,
    pub view_count: usize,
    pub window_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowGraph {
    next_content: u64,
    next_view: u64,
    next_window: u64,
    next_tab: u64,
    contents: BTreeMap<ContentId, ContentDescriptor>,
    views: BTreeMap<ViewId, ViewDescriptor>,
    windows: BTreeMap<WindowId, WindowDescriptor>,
    workspace_tabs: BTreeMap<WorkspaceTabId, WorkspaceTabDescriptor>,
    active_tab: Option<WorkspaceTabId>,
    focused: Option<WindowId>,
    revision: u64,
}

impl WindowGraph {
    /// Monotonic graph/compositor source revision. Exhaustion deliberately
    /// returns `None`, forcing every consumer to fail closed.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        if self.revision == u64::MAX {
            None
        } else {
            Some(self.revision)
        }
    }

    fn ensure_revision_capacity(&self) -> Result<(), WindowError> {
        if self.revision == u64::MAX {
            Err(WindowError::RevisionExhausted)
        } else {
            Ok(())
        }
    }

    fn mark_changed(&mut self) -> Result<(), WindowError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(WindowError::RevisionExhausted)?;
        Ok(())
    }

    fn same_source_state(&self, other: &Self) -> bool {
        self.next_content == other.next_content
            && self.next_view == other.next_view
            && self.next_window == other.next_window
            && self.next_tab == other.next_tab
            && self.contents == other.contents
            && self.views == other.views
            && self.windows == other.windows
            && self.workspace_tabs == other.workspace_tabs
            && self.active_tab == other.active_tab
            && self.focused == other.focused
    }

    #[cfg(test)]
    fn exhaust_revision_for_test(&mut self) {
        self.revision = u64::MAX;
    }
    pub fn create_content(
        &mut self,
        content_type: ContentTypeId,
        adapter: ContentAdapterKind,
        title: String,
        dirty: bool,
        owner: impl Into<String>,
        generation: u64,
    ) -> Result<ContentId, WindowError> {
        self.ensure_revision_capacity()?;
        let id = ContentId(increment(&mut self.next_content)?);
        self.register_content(ContentDescriptor {
            id,
            content_type,
            adapter,
            title,
            dirty,
            owner: owner.into(),
            generation,
        })?;
        Ok(id)
    }

    pub fn register_content(&mut self, descriptor: ContentDescriptor) -> Result<(), WindowError> {
        validate_identity(descriptor.id.0)?;
        validate_owner(&descriptor.owner, descriptor.generation)?;
        validate_title(&descriptor.title)?;
        if self.contents.contains_key(&descriptor.id) {
            return Err(WindowError::DuplicateContent);
        }
        self.ensure_revision_capacity()?;
        self.next_content = self.next_content.max(descriptor.id.0);
        self.contents.insert(descriptor.id, descriptor);
        self.mark_changed()
    }

    pub fn update_content(
        &mut self,
        content: ContentId,
        title: String,
        dirty: bool,
    ) -> Result<(), WindowError> {
        validate_title(&title)?;
        let current = self
            .contents
            .get(&content)
            .ok_or(WindowError::UnknownContent)?;
        if current.title == title && current.dirty == dirty {
            return Ok(());
        }
        self.ensure_revision_capacity()?;
        let descriptor = self
            .contents
            .get_mut(&content)
            .ok_or(WindowError::UnknownContent)?;
        descriptor.title = title;
        descriptor.dirty = dirty;
        self.mark_changed()
    }

    pub fn create_view(
        &mut self,
        content: ContentId,
        owner: impl Into<String>,
        generation: u64,
    ) -> Result<ViewId, WindowError> {
        if !self.contents.contains_key(&content) {
            return Err(WindowError::UnknownContent);
        }
        let owner = owner.into();
        validate_owner(&owner, generation)?;
        self.ensure_revision_capacity()?;
        let id = ViewId(increment(&mut self.next_view)?);
        self.views.insert(
            id,
            ViewDescriptor {
                id,
                content,
                owner,
                generation,
            },
        );
        self.mark_changed()?;
        Ok(id)
    }

    pub fn create_workspace_tab(
        &mut self,
        owner: impl Into<String>,
        generation: u64,
    ) -> Result<WorkspaceTabId, WindowError> {
        let owner = owner.into();
        validate_owner(&owner, generation)?;
        self.ensure_revision_capacity()?;
        let id = WorkspaceTabId(increment(&mut self.next_tab)?);
        self.workspace_tabs.insert(
            id,
            WorkspaceTabDescriptor {
                id,
                tiled_root: None,
                owner,
                generation,
            },
        );
        self.active_tab.get_or_insert(id);
        self.mark_changed()?;
        Ok(id)
    }

    pub fn set_tiled_layout(
        &mut self,
        tab: WorkspaceTabId,
        root: Option<TiledLayoutNode>,
    ) -> Result<(), WindowError> {
        if self
            .workspace_tabs
            .get(&tab)
            .is_some_and(|descriptor| descriptor.tiled_root == root)
        {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            if !candidate.workspace_tabs.contains_key(&tab) {
                return Err(WindowError::UnknownWorkspaceTab);
            }
            validate_tiled_layout(tab, root.as_ref(), &candidate.windows)?;
            candidate
                .workspace_tabs
                .get_mut(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?
                .tiled_root = root;
            Ok(())
        })
    }

    #[must_use]
    pub fn tiled_layout_snapshot(&self, tab: WorkspaceTabId) -> Option<TiledLayoutSnapshot> {
        let descriptor = self.workspace_tabs.get(&tab)?;
        Some(TiledLayoutSnapshot {
            root: descriptor.tiled_root.clone()?,
            focused: self.focused.filter(|focused| {
                self.windows
                    .get(focused)
                    .is_some_and(|window| window.tab == tab)
            }),
        })
    }

    #[must_use]
    pub fn compositor_snapshot(&self, tab: WorkspaceTabId) -> Option<CompositorSnapshot> {
        self.workspace_tabs.get(&tab)?;
        let windows = self
            .windows
            .values()
            .filter(|window| window.tab == tab)
            .filter_map(|window| {
                let view = self.views.get(&window.view)?;
                let content = self.contents.get(&view.content)?;
                Some(CompositorWindowSnapshot {
                    window: window.id,
                    placement: window.placement,
                    adapter: content.adapter,
                    focusable: window.focusable,
                })
            })
            .collect();
        Some(CompositorSnapshot {
            tab,
            tiled: self.tiled_layout_snapshot(tab),
            windows,
            focused: self.focused.filter(|focused| {
                self.windows
                    .get(focused)
                    .is_some_and(|window| window.tab == tab)
            }),
        })
    }

    pub fn resize_tiled_split(
        &mut self,
        tab: WorkspaceTabId,
        window: WindowId,
        delta: i16,
    ) -> Result<(), WindowError> {
        if delta == 0 {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            let root = candidate
                .workspace_tabs
                .get_mut(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?
                .tiled_root
                .as_mut()
                .ok_or(WindowError::MissingTiledLayout)?;
            if !resize_tiled_parent(root, window, delta) {
                return Err(WindowError::WindowNotInTiledLayout);
            }
            Ok(())
        })
    }

    pub fn swap_tiled_windows(
        &mut self,
        tab: WorkspaceTabId,
        first: WindowId,
        second: WindowId,
    ) -> Result<(), WindowError> {
        if first == second {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            let root = candidate
                .workspace_tabs
                .get_mut(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?
                .tiled_root
                .as_mut()
                .ok_or(WindowError::MissingTiledLayout)?;
            if !tiled_contains(root, first) || !tiled_contains(root, second) {
                return Err(WindowError::WindowNotInTiledLayout);
            }
            replace_tiled_window(root, first, WindowId(0));
            replace_tiled_window(root, second, first);
            replace_tiled_window(root, WindowId(0), second);
            Ok(())
        })
    }

    pub fn open_window(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
        owner: impl Into<String>,
        generation: u64,
    ) -> Result<WindowId, WindowError> {
        let owner = owner.into();
        self.checked_mutation(move |candidate| {
            candidate.open_window_inner(tab, view, placement, focusable, owner, generation)
        })
    }

    fn open_window_inner(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
        owner: String,
        generation: u64,
    ) -> Result<WindowId, WindowError> {
        if !self.workspace_tabs.contains_key(&tab) {
            return Err(WindowError::UnknownWorkspaceTab);
        }
        if !self.views.contains_key(&view) {
            return Err(WindowError::UnknownView);
        }
        if self.windows.values().any(|window| window.view == view) {
            return Err(WindowError::ViewAlreadyHosted);
        }
        let placement = placement.validate()?;
        validate_owner(&owner, generation)?;
        let id = WindowId(increment(&mut self.next_window)?);
        self.windows.insert(
            id,
            WindowDescriptor {
                id,
                tab,
                view,
                placement,
                focusable,
                owner,
                generation,
            },
        );
        if placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), id);
        }
        if focusable && placement != WindowPlacement::Hidden {
            self.active_tab = Some(tab);
            self.focused = Some(id);
        }
        Ok(id)
    }

    pub fn rehost(
        &mut self,
        window: WindowId,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
    ) -> Result<(), WindowError> {
        if self.windows.get(&window).is_some_and(|descriptor| {
            descriptor.tab == tab
                && descriptor.view == view
                && descriptor.placement == placement
                && descriptor.focusable == focusable
        }) {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            candidate.rehost_inner(window, tab, view, placement, focusable)
        })
    }

    fn rehost_inner(
        &mut self,
        window: WindowId,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
    ) -> Result<(), WindowError> {
        if !self.workspace_tabs.contains_key(&tab) {
            return Err(WindowError::UnknownWorkspaceTab);
        }
        if !self.views.contains_key(&view) {
            return Err(WindowError::UnknownView);
        }
        if self
            .windows
            .iter()
            .any(|(id, candidate)| *id != window && candidate.view == view)
        {
            return Err(WindowError::ViewAlreadyHosted);
        }
        let placement = placement.validate()?;
        let (previous_tab, previous_placement) = self
            .windows
            .get(&window)
            .map(|descriptor| (descriptor.tab, descriptor.placement))
            .ok_or(WindowError::UnknownWindow)?;
        if matches!(previous_placement, WindowPlacement::Tiled)
            && let Some(previous) = self.workspace_tabs.get_mut(&previous_tab)
        {
            previous.tiled_root = previous
                .tiled_root
                .take()
                .and_then(|root| remove_tiled_window(root, window));
        }
        let descriptor = self
            .windows
            .get_mut(&window)
            .ok_or(WindowError::UnknownWindow)?;
        descriptor.tab = tab;
        descriptor.view = view;
        descriptor.placement = placement;
        descriptor.focusable = focusable;
        if placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), window);
        }
        if self.focused == Some(window) {
            if !focusable || placement == WindowPlacement::Hidden {
                self.focused = None;
                self.select_focus_fallback();
            } else {
                self.active_tab = Some(tab);
            }
        }
        Ok(())
    }

    pub fn place(
        &mut self,
        window: WindowId,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        if self
            .windows
            .get(&window)
            .is_some_and(|descriptor| descriptor.placement == placement)
        {
            return Ok(());
        }
        self.checked_mutation(|candidate| candidate.place_inner(window, placement))
    }

    fn place_inner(
        &mut self,
        window: WindowId,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        let placement = placement.validate()?;
        let (tab, previous_placement) = self
            .windows
            .get(&window)
            .map(|descriptor| (descriptor.tab, descriptor.placement))
            .ok_or(WindowError::UnknownWindow)?;
        if matches!(previous_placement, WindowPlacement::Tiled)
            && !matches!(placement, WindowPlacement::Tiled)
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
        {
            workspace_tab.tiled_root = workspace_tab
                .tiled_root
                .take()
                .and_then(|root| remove_tiled_window(root, window));
        }
        let descriptor = self
            .windows
            .get_mut(&window)
            .ok_or(WindowError::UnknownWindow)?;
        descriptor.placement = placement;
        if !matches!(previous_placement, WindowPlacement::Tiled)
            && placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), window);
        }
        if placement == WindowPlacement::Hidden && self.focused == Some(window) {
            self.focused = None;
            self.select_focus_fallback();
        }
        Ok(())
    }

    /// Atomically exchange two ordinary non-tiled placements without changing
    /// content, view, window, or workspace-tab identity.
    pub fn swap_non_tiled_placements(
        &mut self,
        first: WindowId,
        second: WindowId,
    ) -> Result<(), WindowError> {
        if first == second {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            let first_placement = candidate
                .windows
                .get(&first)
                .map(|window| window.placement)
                .ok_or(WindowError::UnknownWindow)?;
            let second_placement = candidate
                .windows
                .get(&second)
                .map(|window| window.placement)
                .ok_or(WindowError::UnknownWindow)?;
            if matches!(first_placement, WindowPlacement::Tiled)
                || matches!(second_placement, WindowPlacement::Tiled)
            {
                return Err(WindowError::TiledMutationNotAuthoritative);
            }
            candidate
                .windows
                .get_mut(&first)
                .ok_or(WindowError::UnknownWindow)?
                .placement = second_placement;
            candidate
                .windows
                .get_mut(&second)
                .ok_or(WindowError::UnknownWindow)?
                .placement = first_placement;
            if candidate.focused.is_some_and(|focused| {
                candidate
                    .windows
                    .get(&focused)
                    .is_some_and(|window| window.placement == WindowPlacement::Hidden)
            }) {
                candidate.focused = None;
                candidate.select_focus_fallback();
            }
            Ok(())
        })
    }

    pub fn focus(&mut self, window: WindowId) -> Result<(), WindowError> {
        if self.focused == Some(window) {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            let descriptor = candidate
                .windows
                .get(&window)
                .ok_or(WindowError::UnknownWindow)?;
            if !descriptor.focusable || descriptor.placement == WindowPlacement::Hidden {
                return Err(WindowError::NotFocusable);
            }
            candidate.active_tab = Some(descriptor.tab);
            candidate.focused = Some(window);
            Ok(())
        })
    }

    pub fn set_active_tab(&mut self, tab: WorkspaceTabId) -> Result<(), WindowError> {
        if self.active_tab == Some(tab)
            && self
                .focused
                .and_then(|focused| self.windows.get(&focused))
                .is_some_and(|window| window.tab == tab)
        {
            return Ok(());
        }
        self.checked_mutation(|candidate| {
            if !candidate.workspace_tabs.contains_key(&tab) {
                return Err(WindowError::UnknownWorkspaceTab);
            }
            candidate.active_tab = Some(tab);
            if !candidate
                .focused
                .and_then(|focused| candidate.windows.get(&focused))
                .is_some_and(|window| window.tab == tab)
            {
                candidate.focused = candidate
                    .windows
                    .values()
                    .find(|window| {
                        window.tab == tab
                            && window.focusable
                            && window.placement != WindowPlacement::Hidden
                    })
                    .map(|window| window.id);
            }
            Ok(())
        })
    }

    pub fn close_window(&mut self, window: WindowId) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| candidate.close_window_inner(window))
    }

    fn close_window_inner(&mut self, window: WindowId) -> Result<(), WindowError> {
        let removed = self
            .windows
            .remove(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if matches!(removed.placement, WindowPlacement::Tiled)
            && let Some(tab) = self.workspace_tabs.get_mut(&removed.tab)
        {
            tab.tiled_root = tab
                .tiled_root
                .take()
                .and_then(|root| remove_tiled_window(root, window));
        }
        if self.focused == Some(window) {
            self.focused = None;
            self.select_focus_fallback();
        }
        Ok(())
    }

    pub fn remove_view(&mut self, view: ViewId) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            if candidate.windows.values().any(|window| window.view == view) {
                return Err(WindowError::ViewStillHosted);
            }
            candidate
                .views
                .remove(&view)
                .ok_or(WindowError::UnknownView)?;
            Ok(())
        })
    }

    pub fn remove_content(&mut self, content: ContentId) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            if candidate.views.values().any(|view| view.content == content) {
                return Err(WindowError::ContentStillViewed);
            }
            candidate
                .contents
                .remove(&content)
                .ok_or(WindowError::UnknownContent)?;
            Ok(())
        })
    }

    pub fn remove_workspace_tab(&mut self, tab: WorkspaceTabId) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            if candidate.windows.values().any(|window| window.tab == tab) {
                return Err(WindowError::WorkspaceTabStillHosted);
            }
            candidate
                .workspace_tabs
                .remove(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?;
            if candidate.active_tab == Some(tab) {
                candidate.active_tab = candidate.workspace_tabs.keys().next().copied();
            }
            if let Some(focused_tab) = candidate
                .focused
                .and_then(|focused| candidate.windows.get(&focused))
                .map(|window| window.tab)
            {
                candidate.active_tab = Some(focused_tab);
            } else {
                candidate.select_focus_fallback();
            }
            Ok(())
        })
    }

    pub fn remove_owner_generation(
        &mut self,
        owner: &str,
        generation: u64,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            candidate.remove_owner_generation_inner(owner, generation);
            Ok(())
        })
    }

    fn remove_owner_generation_inner(&mut self, owner: &str, generation: u64) {
        let removed_contents = self
            .contents
            .iter()
            .filter_map(|(id, value)| {
                (value.owner == owner && value.generation == generation).then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_views = self
            .views
            .iter()
            .filter_map(|(id, value)| {
                ((value.owner == owner && value.generation == generation)
                    || removed_contents.contains(&value.content))
                .then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_tabs = self
            .workspace_tabs
            .iter()
            .filter_map(|(id, value)| {
                (value.owner == owner && value.generation == generation).then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_windows = self
            .windows
            .iter()
            .filter_map(|(id, value)| {
                ((value.owner == owner && value.generation == generation)
                    || removed_views.contains(&value.view)
                    || removed_tabs.contains(&value.tab))
                .then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        self.windows.retain(|id, _| !removed_windows.contains(id));
        for tab in self.workspace_tabs.values_mut() {
            for window in &removed_windows {
                tab.tiled_root = tab
                    .tiled_root
                    .take()
                    .and_then(|root| remove_tiled_window(root, *window));
            }
        }
        self.views.retain(|id, _| !removed_views.contains(id));
        self.contents.retain(|id, _| !removed_contents.contains(id));
        self.workspace_tabs
            .retain(|id, _| !removed_tabs.contains(id));
        if self
            .focused
            .is_some_and(|focused| !self.windows.contains_key(&focused))
        {
            self.focused = None;
        }
        if self
            .active_tab
            .is_some_and(|tab| !self.workspace_tabs.contains_key(&tab))
        {
            self.active_tab = self.workspace_tabs.keys().next().copied();
        }
        if let Some(focused_tab) = self
            .focused
            .and_then(|focused| self.windows.get(&focused))
            .map(|window| window.tab)
        {
            self.active_tab = Some(focused_tab);
        } else {
            self.select_focus_fallback();
        }
    }

    fn checked_mutation<T>(
        &mut self,
        mutation: impl FnOnce(&mut Self) -> Result<T, WindowError>,
    ) -> Result<T, WindowError> {
        let mut candidate = self.clone();
        let output = mutation(&mut candidate)?;
        candidate.validate()?;
        if !candidate.same_source_state(self) {
            candidate.ensure_revision_capacity()?;
            candidate.mark_changed()?;
            *self = candidate;
        }
        Ok(output)
    }

    pub fn validate(&self) -> Result<(), WindowError> {
        let mut hosted = BTreeSet::new();
        for view in self.views.values() {
            if !self.contents.contains_key(&view.content) {
                return Err(WindowError::UnknownContent);
            }
        }
        for window in self.windows.values() {
            if !self.workspace_tabs.contains_key(&window.tab) {
                return Err(WindowError::UnknownWorkspaceTab);
            }
            if !self.views.contains_key(&window.view) {
                return Err(WindowError::UnknownView);
            }
            if !hosted.insert(window.view) {
                return Err(WindowError::ViewAlreadyHosted);
            }
            window.placement.validate()?;
        }
        for (tab, descriptor) in &self.workspace_tabs {
            validate_tiled_layout(*tab, descriptor.tiled_root.as_ref(), &self.windows)?;
            let non_tiled = self
                .windows
                .values()
                .filter(|window| window.tab == *tab && window.placement != WindowPlacement::Tiled)
                .count();
            if non_tiled > MAXIMUM_NON_TILED_WINDOWS_PER_TAB {
                return Err(WindowError::TooManyNonTiledWindows);
            }
            let fullscreen = self
                .windows
                .values()
                .filter(|window| {
                    window.tab == *tab && window.placement == WindowPlacement::Fullscreen
                })
                .count();
            if fullscreen > 1 {
                return Err(WindowError::MultipleFullscreenWindows);
            }
            let overlays = self
                .windows
                .values()
                .filter(|window| {
                    window.tab == *tab && matches!(window.placement, WindowPlacement::Overlay(_))
                })
                .count();
            if overlays > MAXIMUM_OVERLAY_WINDOWS_PER_TAB {
                return Err(WindowError::TooManyOverlayWindows);
            }
        }
        if let Some(tab) = self.active_tab
            && !self.workspace_tabs.contains_key(&tab)
        {
            return Err(WindowError::UnknownWorkspaceTab);
        }
        if let Some(focused) = self.focused {
            let window = self
                .windows
                .get(&focused)
                .ok_or(WindowError::UnknownWindow)?;
            if !window.focusable || window.placement == WindowPlacement::Hidden {
                return Err(WindowError::NotFocusable);
            }
            if self.active_tab != Some(window.tab) {
                return Err(WindowError::FocusOutsideActiveTab);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn resolve_target(&self, target: &WindowTarget) -> Result<WindowId, WindowError> {
        let window = match target {
            WindowTarget::Focused => self.focused.ok_or(WindowError::UnknownWindow)?,
            WindowTarget::Id(window) => *window,
            WindowTarget::ContentType(content_type) => {
                let active_tab = self.active_tab.ok_or(WindowError::UnknownWorkspaceTab)?;
                let mut matches = self.windows.values().filter_map(|window| {
                    let view = self.views.get(&window.view)?;
                    let content = self.contents.get(&view.content)?;
                    (window.tab == active_tab && &content.content_type == content_type)
                        .then_some(window.id)
                });
                let first = matches.next().ok_or(WindowError::UnknownWindow)?;
                if matches.next().is_some() {
                    return Err(WindowError::AmbiguousWindowTarget);
                }
                first
            }
        };
        let descriptor = self
            .windows
            .get(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if self.active_tab != Some(descriptor.tab) {
            return Err(WindowError::WindowOutsideActiveTab);
        }
        Ok(window)
    }

    pub fn content_snapshots(&self) -> Vec<ContentSnapshot> {
        let active_content = self.focused.and_then(|window| {
            self.windows
                .get(&window)
                .and_then(|window| self.views.get(&window.view))
                .map(|view| view.content)
        });
        self.contents
            .values()
            .map(|content| {
                let view_ids = self
                    .views
                    .values()
                    .filter_map(|view| (view.content == content.id).then_some(view.id))
                    .collect::<BTreeSet<_>>();
                let window_count = self
                    .windows
                    .values()
                    .filter(|window| view_ids.contains(&window.view))
                    .count();
                ContentSnapshot {
                    id: content.id,
                    content_type: content.content_type.clone(),
                    adapter: content.adapter,
                    title: content.title.clone(),
                    dirty: content.dirty,
                    active: active_content == Some(content.id),
                    view_count: view_ids.len(),
                    window_count,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn content(&self, id: ContentId) -> Option<&ContentDescriptor> {
        self.contents.get(&id)
    }

    #[must_use]
    pub fn view(&self, id: ViewId) -> Option<&ViewDescriptor> {
        self.views.get(&id)
    }

    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&WindowDescriptor> {
        self.windows.get(&id)
    }

    #[must_use]
    pub fn workspace_tab(&self, id: WorkspaceTabId) -> Option<&WorkspaceTabDescriptor> {
        self.workspace_tabs.get(&id)
    }

    #[must_use]
    pub const fn active_tab(&self) -> Option<WorkspaceTabId> {
        self.active_tab
    }

    #[must_use]
    pub const fn focused(&self) -> Option<WindowId> {
        self.focused
    }

    #[must_use]
    pub fn tab_has_windows(&self, tab: WorkspaceTabId) -> bool {
        self.windows.values().any(|window| window.tab == tab)
    }

    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.contents.len(),
            self.views.len(),
            self.windows.len(),
            self.workspace_tabs.len(),
        )
    }

    fn select_focus_fallback(&mut self) {
        if self.focused.is_some() {
            return;
        }
        let candidate = self
            .windows
            .values()
            .filter(|window| window.focusable && window.placement != WindowPlacement::Hidden)
            .find(|window| self.active_tab == Some(window.tab))
            .or_else(|| {
                self.windows
                    .values()
                    .find(|window| window.focusable && window.placement != WindowPlacement::Hidden)
            });
        if let Some(window) = candidate {
            self.active_tab = Some(window.tab);
            self.focused = Some(window.id);
        }
    }
}

fn validate_tiled_layout(
    tab: WorkspaceTabId,
    root: Option<&TiledLayoutNode>,
    windows: &BTreeMap<WindowId, WindowDescriptor>,
) -> Result<(), WindowError> {
    fn collect(
        node: &TiledLayoutNode,
        tab: WorkspaceTabId,
        windows: &BTreeMap<WindowId, WindowDescriptor>,
        depth: usize,
        leaves: &mut BTreeSet<WindowId>,
    ) -> Result<(), WindowError> {
        if depth > MAXIMUM_TILED_DEPTH {
            return Err(WindowError::TiledLayoutTooDeep);
        }
        match node {
            TiledLayoutNode::Window(window) => {
                let descriptor = windows.get(window).ok_or(WindowError::StaleTiledWindow)?;
                if descriptor.tab != tab || !matches!(descriptor.placement, WindowPlacement::Tiled)
                {
                    return Err(WindowError::NonTiledWindowInLayout);
                }
                if !leaves.insert(*window) {
                    return Err(WindowError::DuplicateTiledWindow);
                }
                if leaves.len() > MAXIMUM_TILED_WINDOWS {
                    return Err(WindowError::TooManyTiledWindows);
                }
            }
            TiledLayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !(1..=999).contains(ratio) {
                    return Err(WindowError::InvalidPlacement);
                }
                collect(first, tab, windows, depth + 1, leaves)?;
                collect(second, tab, windows, depth + 1, leaves)?;
            }
        }
        Ok(())
    }

    let expected = windows
        .values()
        .filter_map(|window| {
            (window.tab == tab && matches!(window.placement, WindowPlacement::Tiled))
                .then_some(window.id)
        })
        .collect::<BTreeSet<_>>();
    if expected.len() > MAXIMUM_TILED_WINDOWS {
        return Err(WindowError::TooManyTiledWindows);
    }
    let mut leaves = BTreeSet::new();
    if let Some(root) = root {
        collect(root, tab, windows, 1, &mut leaves)?;
    }
    if leaves != expected {
        return Err(WindowError::OrphanTiledWindow);
    }
    Ok(())
}

fn tiled_contains(node: &TiledLayoutNode, target: WindowId) -> bool {
    match node {
        TiledLayoutNode::Window(window) => *window == target,
        TiledLayoutNode::Split { first, second, .. } => {
            tiled_contains(first, target) || tiled_contains(second, target)
        }
    }
}

fn append_tiled_window(root: Option<TiledLayoutNode>, window: WindowId) -> Option<TiledLayoutNode> {
    Some(match root {
        None => TiledLayoutNode::Window(window),
        Some(root) => TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 500,
            first: Box::new(root),
            second: Box::new(TiledLayoutNode::Window(window)),
        },
    })
}

fn remove_tiled_window(node: TiledLayoutNode, target: WindowId) -> Option<TiledLayoutNode> {
    match node {
        TiledLayoutNode::Window(window) => (window != target).then_some(node),
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            remove_tiled_window(*first, target),
            remove_tiled_window(*second, target),
        ) {
            (Some(first), Some(second)) => Some(TiledLayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(node), None) | (None, Some(node)) => Some(node),
            (None, None) => None,
        },
    }
}

fn replace_tiled_window(node: &mut TiledLayoutNode, from: WindowId, to: WindowId) {
    match node {
        TiledLayoutNode::Window(window) if *window == from => *window = to,
        TiledLayoutNode::Window(_) => {}
        TiledLayoutNode::Split { first, second, .. } => {
            replace_tiled_window(first, from, to);
            replace_tiled_window(second, from, to);
        }
    }
}

fn resize_tiled_parent(node: &mut TiledLayoutNode, target: WindowId, delta: i16) -> bool {
    let TiledLayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    if resize_tiled_parent(first, target, delta) || resize_tiled_parent(second, target, delta) {
        return true;
    }
    let signed = if tiled_contains(first, target) {
        delta
    } else if tiled_contains(second, target) {
        -delta
    } else {
        return false;
    };
    let adjusted = i32::from(*ratio) + i32::from(signed);
    *ratio = u16::try_from(adjusted.clamp(1, 999)).unwrap_or(*ratio);
    true
}

fn push_compositor_window(
    output: &mut CompositorProjection,
    window: CompositorWindowSnapshot,
    rect: WindowRect,
    layer: CompositorLayer,
    focused: Option<WindowId>,
) {
    let z_index = u16::try_from(output.windows.len()).unwrap_or(u16::MAX);
    output.windows.push(ProjectedCompositorWindow {
        window: window.window,
        rect,
        layer,
        z_index,
        focused: focused == Some(window.window),
        focusable: window.focusable,
    });
}

fn spatial_score(
    source: WindowRect,
    candidate: WindowRect,
    direction: SpatialDirection,
) -> Option<(u32, u32)> {
    let source_left = u32::from(source.x);
    let source_top = u32::from(source.y);
    let source_right = source_left.saturating_add(u32::from(source.width));
    let source_bottom = source_top.saturating_add(u32::from(source.height));
    let candidate_left = u32::from(candidate.x);
    let candidate_top = u32::from(candidate.y);
    let candidate_right = candidate_left.saturating_add(u32::from(candidate.width));
    let candidate_bottom = candidate_top.saturating_add(u32::from(candidate.height));
    let source_center_x = source_left.saturating_add(u32::from(source.width) / 2);
    let source_center_y = source_top.saturating_add(u32::from(source.height) / 2);
    let candidate_center_x = candidate_left.saturating_add(u32::from(candidate.width) / 2);
    let candidate_center_y = candidate_top.saturating_add(u32::from(candidate.height) / 2);
    match direction {
        SpatialDirection::Left if candidate_center_x < source_center_x => Some((
            source_left.saturating_sub(candidate_right).max(1),
            source_center_y.abs_diff(candidate_center_y),
        )),
        SpatialDirection::Right if candidate_center_x > source_center_x => Some((
            candidate_left.saturating_sub(source_right).max(1),
            source_center_y.abs_diff(candidate_center_y),
        )),
        SpatialDirection::Up if candidate_center_y < source_center_y => Some((
            source_top.saturating_sub(candidate_bottom).max(1),
            source_center_x.abs_diff(candidate_center_x),
        )),
        SpatialDirection::Down if candidate_center_y > source_center_y => Some((
            candidate_top.saturating_sub(source_bottom).max(1),
            source_center_x.abs_diff(candidate_center_x),
        )),
        _ => None,
    }
}

fn project_dock(
    bounds: WindowRect,
    edge: DockEdge,
    requested: u16,
    reserve_width: u16,
    reserve_height: u16,
) -> (WindowRect, WindowRect) {
    match edge {
        DockEdge::Left => {
            let size = requested.min(bounds.width.saturating_sub(reserve_width));
            (
                WindowRect {
                    width: size,
                    ..bounds
                },
                WindowRect {
                    x: bounds.x.saturating_add(size),
                    width: bounds.width.saturating_sub(size),
                    ..bounds
                },
            )
        }
        DockEdge::Right => {
            let size = requested.min(bounds.width.saturating_sub(reserve_width));
            let remaining = bounds.width.saturating_sub(size);
            (
                WindowRect {
                    x: bounds.x.saturating_add(remaining),
                    width: size,
                    ..bounds
                },
                WindowRect {
                    width: remaining,
                    ..bounds
                },
            )
        }
        DockEdge::Top => {
            let size = requested.min(bounds.height.saturating_sub(reserve_height));
            (
                WindowRect {
                    height: size,
                    ..bounds
                },
                WindowRect {
                    y: bounds.y.saturating_add(size),
                    height: bounds.height.saturating_sub(size),
                    ..bounds
                },
            )
        }
        DockEdge::Bottom => {
            let size = requested.min(bounds.height.saturating_sub(reserve_height));
            let remaining = bounds.height.saturating_sub(size);
            (
                WindowRect {
                    y: bounds.y.saturating_add(remaining),
                    height: size,
                    ..bounds
                },
                WindowRect {
                    height: remaining,
                    ..bounds
                },
            )
        }
    }
}

fn intersect_rect(requested: WindowRect, bounds: WindowRect) -> Option<WindowRect> {
    let left = requested.x.max(bounds.x);
    let top = requested.y.max(bounds.y);
    let right = requested
        .x
        .saturating_add(requested.width)
        .min(bounds.x.saturating_add(bounds.width));
    let bottom = requested
        .y
        .saturating_add(requested.height)
        .min(bounds.y.saturating_add(bounds.height));
    (right > left && bottom > top).then_some(WindowRect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    })
}

fn rect_contains(rect: WindowRect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn project_tiled_node(
    node: &TiledLayoutNode,
    bounds: WindowRect,
    output: &mut TiledLayoutProjection,
) {
    match node {
        TiledLayoutNode::Window(window) => output.windows.push(ProjectedWindow {
            window: *window,
            rect: bounds,
        }),
        TiledLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            TileAxis::Horizontal => {
                let available = bounds.width.saturating_sub(1);
                let first_width = available
                    .saturating_mul(*ratio)
                    .checked_div(1_000)
                    .unwrap_or(0)
                    .min(available);
                let second_width = available.saturating_sub(first_width);
                project_tiled_node(
                    first,
                    WindowRect {
                        width: first_width,
                        ..bounds
                    },
                    output,
                );
                let border_x = bounds.x.saturating_add(first_width);
                output.borders.push(ProjectedBorder {
                    x: border_x,
                    y: bounds.y,
                    length: bounds.height,
                    axis: *axis,
                });
                project_tiled_node(
                    second,
                    WindowRect {
                        x: border_x.saturating_add(1),
                        width: second_width,
                        ..bounds
                    },
                    output,
                );
            }
            TileAxis::Vertical => {
                let available = bounds.height.saturating_sub(1);
                let first_height = available
                    .saturating_mul(*ratio)
                    .checked_div(1_000)
                    .unwrap_or(0)
                    .min(available);
                let second_height = available.saturating_sub(first_height);
                project_tiled_node(
                    first,
                    WindowRect {
                        height: first_height,
                        ..bounds
                    },
                    output,
                );
                let border_y = bounds.y.saturating_add(first_height);
                output.borders.push(ProjectedBorder {
                    x: bounds.x,
                    y: border_y,
                    length: bounds.width,
                    axis: *axis,
                });
                project_tiled_node(
                    second,
                    WindowRect {
                        y: border_y.saturating_add(1),
                        height: second_height,
                        ..bounds
                    },
                    output,
                );
            }
        },
    }
}

fn increment(value: &mut u64) -> Result<u64, WindowError> {
    *value = value
        .checked_add(1)
        .ok_or(WindowError::IdentifierExhausted)?;
    Ok(*value)
}

fn validate_identity(value: u64) -> Result<(), WindowError> {
    (value != 0)
        .then_some(())
        .ok_or(WindowError::InvalidIdentity)
}

fn validate_owner(owner: &str, generation: u64) -> Result<(), WindowError> {
    (!owner.is_empty() && owner.len() <= MAXIMUM_OWNER_BYTES && generation != 0)
        .then_some(())
        .ok_or(WindowError::InvalidOwner)
}

fn validate_title(title: &str) -> Result<(), WindowError> {
    (title.len() <= MAXIMUM_TITLE_BYTES && !title.chars().any(char::is_control))
        .then_some(())
        .ok_or(WindowError::InvalidTitle)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WindowError {
    InvalidContentType(String),
    InvalidIdentity,
    InvalidOwner,
    InvalidTitle,
    InvalidPlacement,
    DuplicateContent,
    UnknownContent,
    UnknownView,
    UnknownWindow,
    UnknownWorkspaceTab,
    WindowOutsideActiveTab,
    AmbiguousWindowTarget,
    ViewAlreadyHosted,
    ViewStillHosted,
    ContentStillViewed,
    WorkspaceTabStillHosted,
    FocusOutsideActiveTab,
    NotFocusable,
    MissingTiledLayout,
    WindowNotInTiledLayout,
    StaleTiledWindow,
    NonTiledWindowInLayout,
    DuplicateTiledWindow,
    OrphanTiledWindow,
    TiledLayoutTooDeep,
    TooManyTiledWindows,
    TooManyNonTiledWindows,
    TooManyOverlayWindows,
    MultipleFullscreenWindows,
    TiledMutationNotAuthoritative,
    RevisionExhausted,
    IdentifierExhausted,
}

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContentType(value) => write!(formatter, "invalid content type `{value}`"),
            Self::InvalidIdentity => formatter.write_str("invalid zero window identity"),
            Self::InvalidOwner => formatter.write_str("invalid window resource owner"),
            Self::InvalidTitle => formatter.write_str("invalid window content title"),
            Self::InvalidPlacement => formatter.write_str("invalid window placement"),
            Self::DuplicateContent => formatter.write_str("duplicate content identity"),
            Self::UnknownContent => formatter.write_str("unknown content identity"),
            Self::UnknownView => formatter.write_str("unknown view identity"),
            Self::UnknownWindow => formatter.write_str("unknown window identity"),
            Self::UnknownWorkspaceTab => formatter.write_str("unknown workspace-tab identity"),
            Self::WindowOutsideActiveTab => {
                formatter.write_str("window target is outside the active workspace tab")
            }
            Self::AmbiguousWindowTarget => {
                formatter.write_str("window content target resolves to multiple windows")
            }
            Self::ViewAlreadyHosted => formatter.write_str("view is already hosted by a window"),
            Self::ViewStillHosted => formatter.write_str("view is still hosted by a window"),
            Self::ContentStillViewed => formatter.write_str("content still owns a view"),
            Self::WorkspaceTabStillHosted => {
                formatter.write_str("workspace tab still owns a window")
            }
            Self::FocusOutsideActiveTab => {
                formatter.write_str("focused window is outside the active workspace tab")
            }
            Self::NotFocusable => formatter.write_str("window is not focusable"),
            Self::MissingTiledLayout => formatter.write_str("workspace tab has no tiled layout"),
            Self::WindowNotInTiledLayout => {
                formatter.write_str("window is absent from the tiled layout")
            }
            Self::StaleTiledWindow => formatter.write_str("tiled layout references a stale window"),
            Self::NonTiledWindowInLayout => {
                formatter.write_str("tiled layout references a non-tiled or foreign window")
            }
            Self::DuplicateTiledWindow => {
                formatter.write_str("tiled layout references one window more than once")
            }
            Self::OrphanTiledWindow => {
                formatter.write_str("tiled window is absent from its workspace-tab layout")
            }
            Self::TiledLayoutTooDeep => {
                formatter.write_str("tiled layout exceeds its depth ceiling")
            }
            Self::TooManyTiledWindows => {
                formatter.write_str("tiled layout exceeds its window ceiling")
            }
            Self::TooManyNonTiledWindows => {
                formatter.write_str("workspace tab exceeds its non-tiled window ceiling")
            }
            Self::TooManyOverlayWindows => {
                formatter.write_str("workspace tab exceeds its overlay window ceiling")
            }
            Self::MultipleFullscreenWindows => {
                formatter.write_str("workspace tab owns more than one fullscreen window")
            }
            Self::TiledMutationNotAuthoritative => {
                formatter.write_str("editor tiled mutation is not yet graph-authoritative")
            }
            Self::RevisionExhausted => {
                formatter.write_str("window graph revision space is exhausted")
            }
            Self::IdentifierExhausted => formatter.write_str("window identifier space exhausted"),
        }
    }
}

impl std::error::Error for WindowError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn balanced_layout(windows: &[WindowId]) -> TiledLayoutNode {
        if windows.len() == 1 {
            return TiledLayoutNode::Window(windows[0]);
        }
        let middle = windows.len() / 2;
        TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 500,
            first: Box::new(balanced_layout(&windows[..middle])),
            second: Box::new(balanced_layout(&windows[middle..])),
        }
    }

    fn graph_with_content() -> (WindowGraph, ContentId, ViewId, WorkspaceTabId) {
        let mut graph = WindowGraph::default();
        let content = graph
            .create_content(
                ContentTypeId::new("builtin.terminal").expect("type"),
                ContentAdapterKind::TerminalSession,
                "shell".into(),
                false,
                "builtin",
                1,
            )
            .expect("content");
        let view = graph.create_view(content, "builtin", 1).expect("view");
        let tab = graph
            .create_workspace_tab("builtin", 1)
            .expect("workspace tab");
        (graph, content, view, tab)
    }

    #[test]
    fn one_content_view_and_window_move_without_identity_replacement() {
        let (mut graph, content, view, tab) = graph_with_content();
        let window = graph
            .open_window(
                tab,
                view,
                WindowPlacement::Docked {
                    edge: DockEdge::Bottom,
                    size: 12,
                },
                true,
                "builtin",
                1,
            )
            .expect("window");
        graph
            .place(
                window,
                WindowPlacement::Floating(WindowRect {
                    x: 5,
                    y: 3,
                    width: 80,
                    height: 24,
                }),
            )
            .expect("float");
        graph
            .place(window, WindowPlacement::Fullscreen)
            .expect("fullscreen");
        assert_eq!(graph.window(window).expect("same window").view, view);
        assert_eq!(graph.view(view).expect("same view").content, content);
        assert_eq!(graph.focused(), Some(window));
        graph.validate().expect("valid graph");
    }

    #[test]
    fn content_can_own_independent_views_but_each_view_has_one_window() {
        let (mut graph, content, first_view, tab) = graph_with_content();
        let second_view = graph.create_view(content, "builtin", 1).expect("view");
        let first = graph
            .open_window(
                tab,
                first_view,
                WindowPlacement::Fullscreen,
                true,
                "builtin",
                1,
            )
            .expect("first");
        graph
            .open_window(
                tab,
                second_view,
                WindowPlacement::Floating(WindowRect {
                    x: 1,
                    y: 1,
                    width: 10,
                    height: 5,
                }),
                true,
                "builtin",
                1,
            )
            .expect("second");
        assert_eq!(
            graph.open_window(
                tab,
                first_view,
                WindowPlacement::Fullscreen,
                true,
                "builtin",
                1,
            ),
            Err(WindowError::ViewAlreadyHosted)
        );
        graph.close_window(first).expect("close first window");
        graph.remove_view(first_view).expect("remove first view");
        assert!(graph.content(content).is_some());
        assert_eq!(graph.content_snapshots()[0].view_count, 1);
    }

    #[test]
    fn tiled_topology_rejects_stale_duplicate_orphan_and_excess_depth() {
        let (mut graph, content, first_view, tab) = graph_with_content();
        let second_view = graph.create_view(content, "builtin", 1).expect("view");
        let first = graph
            .open_window(tab, first_view, WindowPlacement::Tiled, true, "builtin", 1)
            .expect("first");
        let second = graph
            .open_window(tab, second_view, WindowPlacement::Tiled, true, "builtin", 1)
            .expect("second");
        let split = |left, right| TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 300,
            first: Box::new(TiledLayoutNode::Window(left)),
            second: Box::new(TiledLayoutNode::Window(right)),
        };
        assert_eq!(
            graph.set_tiled_layout(tab, Some(split(first, first))),
            Err(WindowError::DuplicateTiledWindow)
        );
        assert_eq!(
            graph.set_tiled_layout(tab, Some(split(first, WindowId(999)))),
            Err(WindowError::StaleTiledWindow)
        );
        assert_eq!(
            graph.set_tiled_layout(tab, Some(TiledLayoutNode::Window(first))),
            Err(WindowError::OrphanTiledWindow)
        );
        let floating_view = graph
            .create_view(content, "builtin", 1)
            .expect("floating view");
        let floating = graph
            .open_window(
                tab,
                floating_view,
                WindowPlacement::Floating(WindowRect {
                    x: 1,
                    y: 1,
                    width: 10,
                    height: 5,
                }),
                true,
                "builtin",
                1,
            )
            .expect("floating window");
        assert_eq!(
            graph.set_tiled_layout(tab, Some(split(first, floating))),
            Err(WindowError::NonTiledWindowInLayout)
        );
        let foreign_view = graph
            .create_view(content, "builtin", 1)
            .expect("foreign view");
        let foreign_tab = graph
            .create_workspace_tab("builtin", 1)
            .expect("foreign tab");
        let foreign = graph
            .open_window(
                foreign_tab,
                foreign_view,
                WindowPlacement::Tiled,
                true,
                "builtin",
                1,
            )
            .expect("foreign tiled window");
        assert_eq!(
            graph.set_tiled_layout(tab, Some(split(first, foreign))),
            Err(WindowError::NonTiledWindowInLayout)
        );
        let mut too_deep = TiledLayoutNode::Window(WindowId(999));
        for _ in 0..MAXIMUM_TILED_DEPTH {
            too_deep = TiledLayoutNode::Split {
                axis: TileAxis::Horizontal,
                ratio: 500,
                first: Box::new(too_deep),
                second: Box::new(TiledLayoutNode::Window(WindowId(998))),
            };
        }
        assert_eq!(
            graph.set_tiled_layout(tab, Some(too_deep)),
            Err(WindowError::TiledLayoutTooDeep)
        );
        graph
            .set_tiled_layout(tab, Some(split(first, second)))
            .expect("valid layout");
        graph.validate().expect("valid graph");
    }

    #[test]
    fn topology_count_ceiling_rejects_even_a_shallow_balanced_tree() {
        let (mut graph, content, first_view, tab) = graph_with_content();
        let mut windows = Vec::new();
        for index in 0..=MAXIMUM_TILED_WINDOWS {
            let view = if index == 0 {
                first_view
            } else {
                let id = ViewId(u64::try_from(index + 1).expect("view id"));
                graph.views.insert(
                    id,
                    ViewDescriptor {
                        id,
                        content,
                        owner: "builtin".into(),
                        generation: 1,
                    },
                );
                graph.next_view = graph.next_view.max(id.0);
                id
            };
            let window = WindowId(u64::try_from(index + 1).expect("window id"));
            graph.windows.insert(
                window,
                WindowDescriptor {
                    id: window,
                    tab,
                    view,
                    placement: WindowPlacement::Tiled,
                    focusable: true,
                    owner: "builtin".into(),
                    generation: 1,
                },
            );
            graph.next_window = graph.next_window.max(window.0);
            windows.push(window);
        }
        graph.workspace_tabs.get_mut(&tab).expect("tab").tiled_root =
            Some(balanced_layout(&windows));
        assert_eq!(graph.validate(), Err(WindowError::TooManyTiledWindows));
    }

    #[test]
    fn checked_topology_mutations_reject_without_partial_graph_changes() {
        let (mut graph, content, first_view, tab) = graph_with_content();
        let mut tiled = Vec::new();
        for index in 0..MAXIMUM_TILED_DEPTH {
            let view = if index == 0 {
                first_view
            } else {
                graph.create_view(content, "builtin", 1).expect("view")
            };
            tiled.push(
                graph
                    .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
                    .expect("window within topology ceiling"),
            );
        }
        graph.validate().expect("ceiling graph remains valid");

        let rejected_view = graph
            .create_view(content, "builtin", 1)
            .expect("extra view");
        let before_open = graph.clone();
        assert_eq!(
            graph.open_window(
                tab,
                rejected_view,
                WindowPlacement::Tiled,
                true,
                "builtin",
                1,
            ),
            Err(WindowError::TiledLayoutTooDeep)
        );
        assert_eq!(graph, before_open);

        let floating_view = graph
            .create_view(content, "builtin", 1)
            .expect("floating view");
        let floating = graph
            .open_window(
                tab,
                floating_view,
                WindowPlacement::Floating(WindowRect {
                    x: 2,
                    y: 2,
                    width: 20,
                    height: 8,
                }),
                true,
                "builtin",
                1,
            )
            .expect("floating window");
        let before_place = graph.clone();
        assert_eq!(
            graph.place(floating, WindowPlacement::Tiled),
            Err(WindowError::TiledLayoutTooDeep)
        );
        assert_eq!(graph, before_place);
        assert_eq!(
            graph.rehost(floating, tab, floating_view, WindowPlacement::Tiled, true,),
            Err(WindowError::TiledLayoutTooDeep)
        );
        assert_eq!(graph, before_place);

        let before_stale = graph.clone();
        assert_eq!(
            graph.resize_tiled_split(tab, WindowId(u64::MAX), 100),
            Err(WindowError::WindowNotInTiledLayout)
        );
        assert_eq!(
            graph.swap_tiled_windows(tab, tiled[0], WindowId(u64::MAX)),
            Err(WindowError::WindowNotInTiledLayout)
        );
        assert_eq!(graph, before_stale);
        graph.validate().expect("all rejected mutations are atomic");
    }

    #[test]
    fn nested_projection_tiny_fallback_resize_and_swap_preserve_window_ids() {
        let (mut graph, content, first_view, tab) = graph_with_content();
        let second_view = graph
            .create_view(content, "builtin", 1)
            .expect("second view");
        let third_view = graph
            .create_view(content, "builtin", 1)
            .expect("third view");
        let open = |graph: &mut WindowGraph, view| {
            graph
                .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
                .expect("window")
        };
        let first = open(&mut graph, first_view);
        let second = open(&mut graph, second_view);
        let third = open(&mut graph, third_view);
        graph
            .set_tiled_layout(
                tab,
                Some(TiledLayoutNode::Split {
                    axis: TileAxis::Horizontal,
                    ratio: 300,
                    first: Box::new(TiledLayoutNode::Window(first)),
                    second: Box::new(TiledLayoutNode::Split {
                        axis: TileAxis::Vertical,
                        ratio: 600,
                        first: Box::new(TiledLayoutNode::Window(second)),
                        second: Box::new(TiledLayoutNode::Window(third)),
                    }),
                }),
            )
            .expect("layout");
        graph.focus(third).expect("focus third");
        let snapshot = graph.tiled_layout_snapshot(tab).expect("snapshot");
        let projected = snapshot.project(
            WindowRect {
                x: 0,
                y: 1,
                width: 100,
                height: 30,
            },
            1,
            1,
        );
        assert_eq!(projected.windows.len(), 3);
        assert_eq!(projected.borders.len(), 2);
        assert_eq!(projected.windows[0].window, first);
        assert!(projected.windows[0].rect.width < projected.windows[1].rect.width);

        let tiny = snapshot.project(
            WindowRect {
                x: 0,
                y: 0,
                width: 8,
                height: 3,
            },
            5,
            2,
        );
        assert!(tiny.collapsed_to_focused);
        assert_eq!(
            tiny.windows,
            vec![ProjectedWindow {
                window: third,
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 3
                },
            }]
        );

        graph.resize_tiled_split(tab, first, 100).expect("resize");
        let resized = graph.tiled_layout_snapshot(tab).expect("resized").project(
            WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            },
            1,
            1,
        );
        assert!(resized.windows[0].rect.width > projected.windows[0].rect.width);
        graph.swap_tiled_windows(tab, first, third).expect("swap");
        let swapped = graph.tiled_layout_snapshot(tab).expect("swapped").project(
            WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            },
            1,
            1,
        );
        assert_eq!(swapped.windows[0].window, third);
        assert!(graph.window(first).is_some() && graph.window(third).is_some());
        graph.validate().expect("valid graph");
    }

    #[test]
    fn compositor_projects_docks_tiles_floats_fullscreen_and_overlays_deterministically() {
        let (mut graph, content, tiled_view, tab) = graph_with_content();
        let open = |graph: &mut WindowGraph, placement| {
            let view = graph.create_view(content, "builtin", 1).expect("view");
            graph
                .open_window(tab, view, placement, true, "builtin", 1)
                .expect("window")
        };
        let tiled = graph
            .open_window(tab, tiled_view, WindowPlacement::Tiled, true, "builtin", 1)
            .expect("tiled");
        let left = open(
            &mut graph,
            WindowPlacement::Docked {
                edge: DockEdge::Left,
                size: 20,
            },
        );
        let bottom = open(
            &mut graph,
            WindowPlacement::Docked {
                edge: DockEdge::Bottom,
                size: 6,
            },
        );
        let floating = open(
            &mut graph,
            WindowPlacement::Floating(WindowRect {
                x: 95,
                y: 28,
                width: 20,
                height: 10,
            }),
        );
        let overlay = open(
            &mut graph,
            WindowPlacement::Overlay(WindowRect {
                x: 2,
                y: 2,
                width: 12,
                height: 4,
            }),
        );
        let snapshot = graph.compositor_snapshot(tab).expect("snapshot");
        let projection = snapshot.project(
            WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            },
            5,
            2,
        );
        assert_eq!(projection.window(left).expect("left").rect.width, 20);
        assert_eq!(projection.window(bottom).expect("bottom").rect.height, 6);
        assert_eq!(
            projection.window(tiled).expect("tile").rect,
            WindowRect {
                x: 20,
                y: 0,
                width: 80,
                height: 24,
            }
        );
        assert_eq!(
            projection.window(floating).expect("float clipped").rect,
            WindowRect {
                x: 95,
                y: 28,
                width: 5,
                height: 2,
            }
        );
        assert_eq!(
            projection.windows.last().expect("top overlay").window,
            overlay
        );
        assert_eq!(projection.hit_test(3, 3), Some(overlay));
        assert_eq!(
            projection.spatial_neighbor(tiled, SpatialDirection::Left),
            Some(left)
        );
        assert_eq!(
            projection.spatial_neighbor(tiled, SpatialDirection::Down),
            Some(bottom)
        );

        let mut tiny = snapshot.project(
            WindowRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
            5,
            2,
        );
        assert_eq!(tiny.workspace_bounds.width, 1);
        assert_eq!(tiny.workspace_bounds.height, 1);
        assert_eq!(
            tiny.window(tiled).expect("tiny tile").rect,
            tiny.tiled_bounds
        );
        let protected = WindowRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        tiny.set_protected_regions([protected]);
        assert_eq!(tiny.protected_regions, vec![protected]);
        assert_eq!(tiny.hit_test(0, 0), None);

        let fullscreen = open(&mut graph, WindowPlacement::Fullscreen);
        let before_second = graph.clone();
        let second_fullscreen_view = graph.create_view(content, "builtin", 1).expect("view");
        let before_rejected = graph.clone();
        assert_eq!(
            graph.open_window(
                tab,
                second_fullscreen_view,
                WindowPlacement::Fullscreen,
                true,
                "builtin",
                1,
            ),
            Err(WindowError::MultipleFullscreenWindows)
        );
        assert_eq!(graph, before_rejected);
        assert_ne!(graph, before_second);
        let fullscreen_projection = graph.compositor_snapshot(tab).expect("snapshot").project(
            WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            },
            5,
            2,
        );
        assert_eq!(fullscreen_projection.windows[0].window, fullscreen);
        assert_eq!(
            fullscreen_projection.windows[0].layer,
            CompositorLayer::Fullscreen
        );
        assert_eq!(
            fullscreen_projection
                .windows
                .last()
                .expect("overlay")
                .window,
            overlay
        );
        assert!(fullscreen_projection.window(tiled).is_none());
        graph.validate().expect("valid compositor graph");
    }

    #[test]
    fn graph_revision_and_typed_targets_cover_changes_rejection_and_exhaustion() {
        let (mut graph, content, view, tab) = graph_with_content();
        let window = graph
            .open_window(
                tab,
                view,
                WindowPlacement::Docked {
                    edge: DockEdge::Left,
                    size: 4,
                },
                true,
                "builtin",
                1,
            )
            .expect("window");
        let opened = graph.revision().expect("revision");
        graph
            .update_content(content, "shell".into(), false)
            .expect("no-op update");
        assert_eq!(graph.revision(), Some(opened));
        graph
            .place(
                window,
                WindowPlacement::Floating(WindowRect {
                    x: 1,
                    y: 2,
                    width: 8,
                    height: 4,
                }),
            )
            .expect("placement");
        assert_eq!(graph.revision(), Some(opened + 1));
        let before_rejection = graph.clone();
        assert_eq!(
            graph.place(
                WindowId(999_999),
                WindowPlacement::Docked {
                    edge: DockEdge::Right,
                    size: 2,
                }
            ),
            Err(WindowError::UnknownWindow)
        );
        assert_eq!(graph, before_rejection);
        assert_eq!(graph.resolve_target(&WindowTarget::Id(window)), Ok(window));
        assert_eq!(
            graph.resolve_target(&WindowTarget::ContentType(
                ContentTypeId::new("builtin.terminal").expect("type")
            )),
            Ok(window)
        );
        let ambiguous_view = graph.create_view(content, "builtin", 1).expect("view");
        let ambiguous = graph
            .open_window(
                tab,
                ambiguous_view,
                WindowPlacement::Floating(WindowRect {
                    x: 2,
                    y: 2,
                    width: 3,
                    height: 2,
                }),
                true,
                "builtin",
                1,
            )
            .expect("ambiguous window");
        assert_eq!(
            graph.resolve_target(&WindowTarget::ContentType(
                ContentTypeId::new("builtin.terminal").expect("type")
            )),
            Err(WindowError::AmbiguousWindowTarget)
        );
        graph.close_window(ambiguous).expect("close ambiguous");
        graph.remove_view(ambiguous_view).expect("remove view");

        let second_tab = graph.create_workspace_tab("builtin", 1).expect("tab");
        let second_view = graph.create_view(content, "builtin", 1).expect("view");
        let second = graph
            .open_window(
                second_tab,
                second_view,
                WindowPlacement::Floating(WindowRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                true,
                "builtin",
                1,
            )
            .expect("second");
        graph.set_active_tab(tab).expect("first tab");
        assert_eq!(
            graph.resolve_target(&WindowTarget::Id(second)),
            Err(WindowError::WindowOutsideActiveTab)
        );

        let mut exhausted = graph.clone();
        exhausted.exhaust_revision_for_test();
        let before = exhausted.clone();
        assert_eq!(
            exhausted.update_content(content, "changed".into(), true),
            Err(WindowError::RevisionExhausted)
        );
        assert_eq!(exhausted, before);
        assert_eq!(exhausted.revision(), None);
    }

    #[test]
    fn generation_cleanup_removes_only_exact_owned_graph_resources() {
        let mut graph = WindowGraph::default();
        let old = graph
            .create_content(
                ContentTypeId::new("plugin.old").expect("type"),
                ContentAdapterKind::DeclarativeUi,
                "old".into(),
                false,
                "plugin",
                1,
            )
            .expect("old");
        let new = graph
            .create_content(
                ContentTypeId::new("plugin.new").expect("type"),
                ContentAdapterKind::DeclarativeUi,
                "new".into(),
                false,
                "plugin",
                2,
            )
            .expect("new");
        for (content, generation) in [(old, 1), (new, 2)] {
            let view = graph
                .create_view(content, "plugin", generation)
                .expect("view");
            let tab = graph
                .create_workspace_tab("plugin", generation)
                .expect("tab");
            graph
                .open_window(
                    tab,
                    view,
                    WindowPlacement::Fullscreen,
                    true,
                    "plugin",
                    generation,
                )
                .expect("window");
        }
        graph
            .remove_owner_generation("plugin", 1)
            .expect("remove owner generation");
        assert!(graph.content(old).is_none());
        assert!(graph.content(new).is_some());
        assert_eq!(graph.counts(), (1, 1, 1, 1));
        graph.validate().expect("valid graph");
    }

    #[test]
    fn ids_placements_focus_and_removal_order_are_validated() {
        assert!(ContentTypeId::new("Plugin.Bad").is_err());
        let (mut graph, content, view, tab) = graph_with_content();
        assert_eq!(
            graph.open_window(
                tab,
                view,
                WindowPlacement::Docked {
                    edge: DockEdge::Left,
                    size: 0,
                },
                true,
                "builtin",
                1,
            ),
            Err(WindowError::InvalidPlacement)
        );
        assert_eq!(
            graph.remove_content(content),
            Err(WindowError::ContentStillViewed)
        );
    }
}
