//! Window topology, focus and resource ownership checked before publication.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::windows::MAXIMUM_NON_TILED_WINDOWS_PER_TAB;

use crate::windows::MAXIMUM_OVERLAY_WINDOWS_PER_TAB;

use crate::windows::error::WindowError;

use crate::windows::error::increment;

use crate::windows::error::validate_identity;

use crate::windows::error::validate_owner;

use crate::windows::error::validate_title;

use crate::windows::identity::ContentId;

use crate::windows::identity::ContentTypeId;

use crate::windows::identity::ViewId;

use crate::windows::identity::WindowId;

use crate::windows::identity::WindowTarget;

use crate::windows::identity::WorkspaceTabId;

use crate::windows::layout::CompositorSnapshot;

use crate::windows::layout::CompositorWindowSnapshot;

use crate::windows::layout::ContentAdapterKind;

use crate::windows::layout::TileAxis;

use crate::windows::layout::TiledLayoutNode;

use crate::windows::layout::TiledLayoutSnapshot;

use crate::windows::layout::WindowPlacement;

use crate::windows::content::ContentCapabilities;

use crate::windows::content::ContentContract;

use crate::windows::content::ContentDescriptor;

use crate::windows::content::ContentLifecycle;

use crate::windows::content::ContentSnapshot;

use crate::windows::content::ViewDescriptor;

use crate::windows::content::WindowDescriptor;

use crate::windows::content::WorkspaceTabDescriptor;

use crate::windows::projection::append_tiled_window;

use crate::windows::projection::remove_tiled_window;

use crate::windows::projection::replace_tiled_window;

use crate::windows::tree::resize_parent;

use crate::windows::tree::contains_window;

use crate::windows::projection::validate_tiled_layout;

#[cfg_attr(not(test), derive(Clone))]
#[derive(Debug, Default, Eq, PartialEq)]
pub struct WindowGraph {
    pub(super) next_content: u64,
    pub(super) next_view: u64,
    pub(super) next_window: u64,
    pub(super) next_tab: u64,
    pub(super) contents: BTreeMap<ContentId, ContentDescriptor>,
    pub(super) views: BTreeMap<ViewId, ViewDescriptor>,
    pub(super) windows: BTreeMap<WindowId, WindowDescriptor>,
    pub(super) workspace_tabs: BTreeMap<WorkspaceTabId, WorkspaceTabDescriptor>,
    pub(super) active_tab: Option<WorkspaceTabId>,
    pub(super) focused: Option<WindowId>,
    pub(super) remembered_focus: BTreeMap<WorkspaceTabId, WindowId>,
    pub(super) owner_focus: BTreeMap<(WorkspaceTabId, String), WindowId>,
    pub(super) revision: u64,
}

#[cfg(test)]
thread_local! { static GRAPH_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
// Test-only accounting mirrors the derived production clone, including the
// copies made inside checked_mutation; no instrumentation enters production.
#[cfg(test)]
impl Clone for WindowGraph {
    fn clone(&self) -> Self {
        GRAPH_CLONES.with(|count| count.set(count.get() + 1));
        Self {
            next_content: self.next_content,
            next_view: self.next_view,
            next_window: self.next_window,
            next_tab: self.next_tab,
            contents: self.contents.clone(),
            views: self.views.clone(),
            windows: self.windows.clone(),
            workspace_tabs: self.workspace_tabs.clone(),
            active_tab: self.active_tab,
            focused: self.focused,
            remembered_focus: self.remembered_focus.clone(),
            owner_focus: self.owner_focus.clone(),
            revision: self.revision,
        }
    }
}
impl WindowGraph {
    #[cfg(test)]
    pub(crate) fn test_clone_count() -> usize {
        GRAPH_CLONES.with(std::cell::Cell::get)
    }
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
            && self.remembered_focus == other.remembered_focus
            && self.owner_focus == other.owner_focus
    }

    #[cfg(test)]
    pub(super) fn exhaust_revision_for_test(&mut self) {
        self.revision = u64::MAX;
    }
    pub fn create_content(
        &mut self,
        content_type: ContentTypeId,
        adapter: ContentAdapterKind,
        title: String,
        dirty: bool,
        owner: impl Into<String>,
        instance_id: u64,
    ) -> Result<ContentId, WindowError> {
        self.create_content_with_contract(
            content_type,
            ContentContract::adapter_defaults(adapter),
            title,
            dirty,
            owner,
            instance_id,
        )
    }

    pub fn create_content_with_contract(
        &mut self,
        content_type: ContentTypeId,
        contract: ContentContract,
        title: String,
        dirty: bool,
        owner: impl Into<String>,
        instance_id: u64,
    ) -> Result<ContentId, WindowError> {
        self.ensure_revision_capacity()?;
        let id = ContentId(increment(&mut self.next_content)?);
        self.register_content(ContentDescriptor {
            id,
            content_type,
            adapter: contract.adapter,
            capabilities: contract.capabilities,
            lifecycle: contract.lifecycle,
            title,
            dirty,
            owner: owner.into(),
            instance_id,
        })?;
        Ok(id)
    }

    pub fn register_content(&mut self, descriptor: ContentDescriptor) -> Result<(), WindowError> {
        validate_identity(descriptor.id.0)?;
        validate_owner(&descriptor.owner, descriptor.instance_id)?;
        validate_title(&descriptor.title)?;
        if self.contents.contains_key(&descriptor.id) {
            return Err(WindowError::DuplicateContent);
        }
        self.ensure_revision_capacity()?;
        self.next_content = self.next_content.max(descriptor.id.0);
        self.contents.insert(descriptor.id, descriptor);
        self.mark_changed()
    }

    pub fn require_selection(&self, content: ContentId) -> Result<(), WindowError> {
        self.require_content_capability(content, "selection", |capabilities| {
            capabilities.selectable
        })
    }

    pub fn require_editing(&self, content: ContentId) -> Result<(), WindowError> {
        self.require_content_capability(content, "editing", |capabilities| capabilities.editable)
    }

    pub fn require_clone_view(&self, content: ContentId) -> Result<(), WindowError> {
        self.require_content_capability(content, "clone-view", |capabilities| {
            capabilities.clone_view
        })
    }

    pub fn require_persist_descriptor(&self, content: ContentId) -> Result<(), WindowError> {
        self.require_content_capability(content, "persist-descriptor", |capabilities| {
            capabilities.persist_descriptor
        })
    }

    pub fn require_stop_resource(&self, content: ContentId) -> Result<(), WindowError> {
        self.require_content_capability(content, "stop-resource", |capabilities| {
            capabilities.stop_resource
        })
    }

    fn require_content_capability(
        &self,
        content: ContentId,
        operation: &'static str,
        permits: impl FnOnce(ContentCapabilities) -> bool,
    ) -> Result<(), WindowError> {
        let capabilities = self
            .contents
            .get(&content)
            .ok_or(WindowError::UnknownContent)?
            .capabilities;
        if permits(capabilities) {
            Ok(())
        } else {
            Err(WindowError::Unsupported(operation))
        }
    }

    pub fn update_content(
        &mut self,
        content: ContentId,
        title: String,
        dirty: bool,
    ) -> Result<(), WindowError> {
        let lifecycle = self
            .contents
            .get(&content)
            .ok_or(WindowError::UnknownContent)?
            .lifecycle;
        self.update_content_lifecycle(content, title, dirty, lifecycle)
    }

    pub fn update_content_lifecycle(
        &mut self,
        content: ContentId,
        title: String,
        dirty: bool,
        lifecycle: ContentLifecycle,
    ) -> Result<(), WindowError> {
        validate_title(&title)?;
        let current = self
            .contents
            .get(&content)
            .ok_or(WindowError::UnknownContent)?;
        if current.title == title && current.dirty == dirty && current.lifecycle == lifecycle {
            return Ok(());
        }
        self.ensure_revision_capacity()?;
        let descriptor = self
            .contents
            .get_mut(&content)
            .ok_or(WindowError::UnknownContent)?;
        descriptor.title = title;
        descriptor.dirty = dirty;
        descriptor.lifecycle = lifecycle;
        self.mark_changed()
    }

    pub fn create_view(
        &mut self,
        content: ContentId,
        owner: impl Into<String>,
        instance_id: u64,
    ) -> Result<ViewId, WindowError> {
        let descriptor = self
            .contents
            .get(&content)
            .ok_or(WindowError::UnknownContent)?;
        if self.views.values().any(|view| view.content == content)
            && !descriptor.capabilities.clone_view
        {
            return Err(WindowError::Unsupported("clone-view"));
        }
        let owner = owner.into();
        validate_owner(&owner, instance_id)?;
        self.ensure_revision_capacity()?;
        let id = ViewId(increment(&mut self.next_view)?);
        self.views.insert(
            id,
            ViewDescriptor {
                id,
                content,
                owner,
                instance_id,
            },
        );
        self.mark_changed()?;
        Ok(id)
    }

    pub fn create_workspace_tab(
        &mut self,
        owner: impl Into<String>,
        instance_id: u64,
    ) -> Result<WorkspaceTabId, WindowError> {
        let owner = owner.into();
        validate_owner(&owner, instance_id)?;
        self.ensure_revision_capacity()?;
        let id = WorkspaceTabId(increment(&mut self.next_tab)?);
        self.workspace_tabs.insert(
            id,
            WorkspaceTabDescriptor {
                id,
                tiled_root: None,
                owner,
                instance_id,
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
            let expected = candidate
                .windows
                .values()
                .filter(|window| window.tab == tab && window.placement == WindowPlacement::Tiled)
                .map(|window| window.id)
                .collect();
            validate_tiled_layout(tab, root.as_ref(), &candidate.windows, &expected)?;
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
            root: descriptor.tiled_root.as_ref()?.filtered(&|window| {
                self.windows
                    .get(&window)
                    .is_some_and(|window| window.placement == WindowPlacement::Tiled)
            })?,
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
            if !resize_parent(root, window, None, delta, 1, 999) {
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
            if !contains_window(root, first) || !contains_window(root, second) {
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
        instance_id: u64,
    ) -> Result<WindowId, WindowError> {
        let owner = owner.into();
        self.checked_mutation(move |candidate| {
            candidate.open_window_inner(tab, view, placement, focusable, owner, instance_id)
        })
    }

    fn open_window_inner(
        &mut self,
        tab: WorkspaceTabId,
        view: ViewId,
        placement: WindowPlacement,
        focusable: bool,
        owner: String,
        instance_id: u64,
    ) -> Result<WindowId, WindowError> {
        if !self.workspace_tabs.contains_key(&tab) {
            return Err(WindowError::UnknownWorkspaceTab);
        }
        let capabilities = self.capabilities_for_view(view)?;
        let placement = placement.validate()?;
        if !capabilities.permits(placement) {
            return Err(WindowError::Unsupported("placement"));
        }
        if focusable && !capabilities.focusable {
            return Err(WindowError::Unsupported("focus"));
        }
        if self.windows.values().any(|window| window.view == view) {
            return Err(WindowError::ViewAlreadyHosted);
        }
        validate_owner(&owner, instance_id)?;
        let id = WindowId(increment(&mut self.next_window)?);
        self.windows.insert(
            id,
            WindowDescriptor {
                id,
                tab,
                view,
                placement,
                restore_placement: if placement == WindowPlacement::Hidden {
                    WindowPlacement::Tiled
                } else {
                    placement
                },
                focusable,
                owner,
                instance_id,
            },
        );
        if placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), id);
        }
        if self.eligible_focus(id, tab) {
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
        let capabilities = self.capabilities_for_view(view)?;
        let placement = placement.validate()?;
        if !capabilities.permits(placement) {
            return Err(WindowError::Unsupported("placement"));
        }
        if focusable && !capabilities.focusable {
            return Err(WindowError::Unsupported("focus"));
        }
        if self
            .windows
            .iter()
            .any(|(id, candidate)| *id != window && candidate.view == view)
        {
            return Err(WindowError::ViewAlreadyHosted);
        }
        let (previous_tab, previous_placement) = self
            .windows
            .get(&window)
            .map(|descriptor| (descriptor.tab, descriptor.placement))
            .ok_or(WindowError::UnknownWindow)?;
        if previous_tab != tab
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
        if placement != WindowPlacement::Hidden {
            descriptor.restore_placement = placement;
        } else if previous_placement != WindowPlacement::Hidden {
            descriptor.restore_placement = previous_placement;
        }
        descriptor.placement = placement;
        descriptor.focusable = focusable;
        if placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
            && workspace_tab
                .tiled_root
                .as_ref()
                .is_none_or(|root| !contains_window(root, window))
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), window);
        }
        if placement == WindowPlacement::Fullscreen && focusable {
            self.active_tab = Some(tab);
            self.focused = Some(window);
        }
        if self.focused == Some(window) {
            if !self.eligible_focus(window, tab) {
                self.focused = None;
                self.select_focus_fallback();
            } else {
                self.active_tab = Some(tab);
            }
        }
        Ok(())
    }

    pub fn tile(
        &mut self,
        window: WindowId,
        neighbor: Option<WindowId>,
        axis: TileAxis,
        after: bool,
        ratio: u16,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            if !(1..=999).contains(&ratio) || neighbor == Some(window) {
                return Err(WindowError::InvalidPlacement);
            }
            let descriptor = candidate
                .windows
                .get(&window)
                .ok_or(WindowError::UnknownWindow)?;
            if !candidate.capabilities_for_view(descriptor.view)?.tile {
                return Err(WindowError::Unsupported("placement"));
            }
            let tab = descriptor.tab;
            if let Some(neighbor) = neighbor {
                let neighbor_descriptor = candidate
                    .windows
                    .get(&neighbor)
                    .ok_or(WindowError::UnknownWindow)?;
                if neighbor_descriptor.tab != tab
                    || neighbor_descriptor.placement != WindowPlacement::Tiled
                {
                    return Err(WindowError::WindowNotInTiledLayout);
                }
            }
            let workspace = candidate
                .workspace_tabs
                .get_mut(&tab)
                .ok_or(WindowError::UnknownWorkspaceTab)?;
            workspace.tiled_root = workspace
                .tiled_root
                .take()
                .and_then(|root| remove_tiled_window(root, window));
            candidate
                .windows
                .get_mut(&window)
                .ok_or(WindowError::UnknownWindow)?
                .placement = WindowPlacement::Tiled;
            workspace.tiled_root = if let Some(neighbor) = neighbor {
                let mut root = workspace
                    .tiled_root
                    .take()
                    .ok_or(WindowError::MissingTiledLayout)?;
                let (first, second) = if after {
                    (neighbor, window)
                } else {
                    (window, neighbor)
                };
                if !replace_tiled_leaf(
                    &mut root,
                    neighbor,
                    TiledLayoutNode::Split {
                        axis,
                        ratio,
                        first: Box::new(TiledLayoutNode::Window(first)),
                        second: Box::new(TiledLayoutNode::Window(second)),
                    },
                ) {
                    return Err(WindowError::WindowNotInTiledLayout);
                }
                Some(root)
            } else {
                workspace.tiled_root.take().map_or_else(
                    || Some(TiledLayoutNode::Window(window)),
                    |root| {
                        let added = TiledLayoutNode::Window(window);
                        let (first, second) = if after { (root, added) } else { (added, root) };
                        Some(TiledLayoutNode::Split {
                            axis,
                            ratio,
                            first: Box::new(first),
                            second: Box::new(second),
                        })
                    },
                )
            };
            Ok(())
        })
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
        let descriptor = self
            .windows
            .get(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if !self
            .capabilities_for_view(descriptor.view)?
            .permits(placement)
        {
            return Err(WindowError::Unsupported("placement"));
        }
        let (tab, previous_placement) = (descriptor.tab, descriptor.placement);
        let descriptor = self
            .windows
            .get_mut(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if placement != WindowPlacement::Hidden {
            descriptor.restore_placement = placement;
        } else if previous_placement != WindowPlacement::Hidden {
            descriptor.restore_placement = previous_placement;
        }
        descriptor.placement = placement;
        if placement == WindowPlacement::Tiled
            && let Some(workspace_tab) = self.workspace_tabs.get_mut(&tab)
            && workspace_tab
                .tiled_root
                .as_ref()
                .is_none_or(|root| !contains_window(root, window))
        {
            workspace_tab.tiled_root = append_tiled_window(workspace_tab.tiled_root.take(), window);
        }
        if placement == WindowPlacement::Fullscreen {
            if self.eligible_focus(window, tab) {
                self.focus(window)?;
            } else if self.focused.is_some_and(|focused| !self.can_focus(focused)) {
                self.focused = None;
                self.select_focus_fallback();
            }
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
            let first_view = candidate
                .windows
                .get(&first)
                .ok_or(WindowError::UnknownWindow)?
                .view;
            let second_view = candidate
                .windows
                .get(&second)
                .ok_or(WindowError::UnknownWindow)?
                .view;
            if !candidate
                .capabilities_for_view(first_view)?
                .permits(second_placement)
                || !candidate
                    .capabilities_for_view(second_view)?
                    .permits(first_placement)
            {
                return Err(WindowError::Unsupported("placement"));
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
            let focus = if first_placement == WindowPlacement::Fullscreen
                || (candidate.focused == Some(first) && second_placement == WindowPlacement::Hidden)
            {
                Some(second)
            } else if second_placement == WindowPlacement::Fullscreen
                || (candidate.focused == Some(second) && first_placement == WindowPlacement::Hidden)
            {
                Some(first)
            } else {
                None
            };
            if let Some(window) = focus.filter(|window| candidate.can_focus(*window)) {
                candidate.focus(window)?;
            }
            if candidate
                .focused
                .is_some_and(|focused| !candidate.can_focus(focused))
            {
                candidate.focused = [first, second].into_iter().find(|window| {
                    candidate.windows[window].placement == WindowPlacement::Fullscreen
                        && candidate.can_focus(*window)
                });
                candidate.select_focus_fallback();
            }
            Ok(())
        })
    }

    #[must_use]
    pub fn can_focus(&self, window: WindowId) -> bool {
        self.windows
            .get(&window)
            .is_some_and(|descriptor| self.eligible_focus(window, descriptor.tab))
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
            if !candidate.eligible_focus(window, descriptor.tab) {
                return Err(WindowError::NotFocusable);
            }
            if !candidate.capabilities_for_view(descriptor.view)?.focusable {
                return Err(WindowError::Unsupported("focus"));
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
            if candidate
                .focused
                .and_then(|focused| candidate.windows.get(&focused))
                .is_none_or(|window| window.tab != tab)
            {
                candidate.focused = candidate
                    .remembered_focus
                    .get(&tab)
                    .copied()
                    .filter(|remembered| candidate.eligible_focus(*remembered, tab))
                    .or_else(|| {
                        candidate
                            .windows
                            .values()
                            .find(|window| candidate.eligible_focus(window.id, tab))
                            .map(|window| window.id)
                    });
            }
            Ok(())
        })
    }

    pub fn close_window(&mut self, window: WindowId) -> Result<(), WindowError> {
        let descriptor = self
            .windows
            .get(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if !self.capabilities_for_view(descriptor.view)?.close_view {
            return Err(WindowError::Unsupported("close-view"));
        }
        self.checked_mutation(|candidate| candidate.close_window_inner(window))
    }

    fn close_window_inner(&mut self, window: WindowId) -> Result<(), WindowError> {
        let removed = self
            .windows
            .remove(&window)
            .ok_or(WindowError::UnknownWindow)?;
        if let Some(tab) = self.workspace_tabs.get_mut(&removed.tab) {
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

    pub fn remove_owner_instance(
        &mut self,
        owner: &str,
        instance_id: u64,
    ) -> Result<(), WindowError> {
        self.checked_mutation(|candidate| {
            candidate.remove_owner_instance_inner(owner, instance_id);
            Ok(())
        })
    }

    fn remove_owner_instance_inner(&mut self, owner: &str, instance_id: u64) {
        let removed_contents = self
            .contents
            .iter()
            .filter_map(|(id, value)| {
                (value.owner == owner && value.instance_id == instance_id).then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_views = self
            .views
            .iter()
            .filter_map(|(id, value)| {
                ((value.owner == owner && value.instance_id == instance_id)
                    || removed_contents.contains(&value.content))
                .then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_tabs = self
            .workspace_tabs
            .iter()
            .filter_map(|(id, value)| {
                (value.owner == owner && value.instance_id == instance_id).then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let removed_windows = self
            .windows
            .iter()
            .filter_map(|(id, value)| {
                ((value.owner == owner && value.instance_id == instance_id)
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

    pub(super) fn checked_mutation<T>(
        &mut self,
        mutation: impl FnOnce(&mut Self) -> Result<T, WindowError>,
    ) -> Result<T, WindowError> {
        let mut candidate = self.clone();
        let output = mutation(&mut candidate)?;
        candidate.synchronize_focus_memory();
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
        let mut tiled: BTreeMap<WorkspaceTabId, BTreeSet<WindowId>> = BTreeMap::new();
        let mut non_tiled: BTreeMap<WorkspaceTabId, usize> = BTreeMap::new();
        let mut overlays: BTreeMap<WorkspaceTabId, usize> = BTreeMap::new();
        let mut fullscreen: BTreeMap<WorkspaceTabId, WindowId> = BTreeMap::new();
        for view in self.views.values() {
            if !self.contents.contains_key(&view.content) {
                return Err(WindowError::UnknownContent);
            }
        }
        for content in self.contents.values() {
            if !content.capabilities.clone_view
                && self
                    .views
                    .values()
                    .filter(|view| view.content == content.id)
                    .nth(1)
                    .is_some()
            {
                return Err(WindowError::Unsupported("clone-view"));
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
            if window.placement == WindowPlacement::Tiled {
                tiled.entry(window.tab).or_default().insert(window.id);
            } else {
                *non_tiled.entry(window.tab).or_default() += 1;
            }
            if window.placement == WindowPlacement::Fullscreen
                && fullscreen.insert(window.tab, window.id).is_some()
            {
                return Err(WindowError::MultipleFullscreenWindows);
            }
            if matches!(window.placement, WindowPlacement::Overlay(_)) {
                *overlays.entry(window.tab).or_default() += 1;
            }
            let capabilities = self.capabilities_for_view(window.view)?;
            if !capabilities.permits(window.placement) {
                return Err(WindowError::Unsupported("placement"));
            }
            if window.focusable && !capabilities.focusable {
                return Err(WindowError::Unsupported("focus"));
            }
        }
        let empty = BTreeSet::new();
        for (tab, descriptor) in &self.workspace_tabs {
            crate::windows::projection::validate_logical_layout(
                *tab,
                descriptor.tiled_root.as_ref(),
                &self.windows,
                tiled.get(tab).unwrap_or(&empty),
            )?;
            if non_tiled.get(tab).copied().unwrap_or(0) > MAXIMUM_NON_TILED_WINDOWS_PER_TAB {
                return Err(WindowError::TooManyNonTiledWindows);
            }
            if overlays.get(tab).copied().unwrap_or(0) > MAXIMUM_OVERLAY_WINDOWS_PER_TAB {
                return Err(WindowError::TooManyOverlayWindows);
            }
        }
        if let Some(tab) = self.active_tab
            && !self.workspace_tabs.contains_key(&tab)
        {
            return Err(WindowError::UnknownWorkspaceTab);
        }
        for (tab, remembered) in &self.remembered_focus {
            if self.windows.get(remembered).is_none_or(|window| {
                window.tab != *tab
                    || !window.focusable
                    || window.placement == WindowPlacement::Hidden
                    || (fullscreen.get(tab).is_some_and(|id| id != remembered)
                        && !matches!(window.placement, WindowPlacement::Overlay(_)))
            }) {
                return Err(WindowError::NotFocusable);
            }
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
            if self.remembered_focus.get(&window.tab) != Some(&focused) {
                return Err(WindowError::NotFocusable);
            }
        }
        Ok(())
    }

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
                    capabilities: content.capabilities,
                    lifecycle: content.lifecycle,
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

    fn capabilities_for_view(&self, id: ViewId) -> Result<ContentCapabilities, WindowError> {
        let view = self.views.get(&id).ok_or(WindowError::UnknownView)?;
        self.contents
            .get(&view.content)
            .map(|content| content.capabilities)
            .ok_or(WindowError::UnknownContent)
    }

    #[must_use]
    pub fn view(&self, id: ViewId) -> Option<&ViewDescriptor> {
        self.views.get(&id)
    }

    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&WindowDescriptor> {
        self.windows.get(&id)
    }

    pub fn windows(&self) -> impl Iterator<Item = &WindowDescriptor> {
        self.windows.values()
    }

    #[must_use]
    pub fn content_for_window(&self, id: WindowId) -> Option<&ContentDescriptor> {
        let view = self.views.get(&self.windows.get(&id)?.view)?;
        self.contents.get(&view.content)
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

    fn eligible_focus(&self, window: WindowId, tab: WorkspaceTabId) -> bool {
        self.windows.get(&window).is_some_and(|window| {
            window.tab == tab
                && window.focusable
                && window.placement != WindowPlacement::Hidden
                && !self.windows.values().any(|other| {
                    !matches!(window.placement, WindowPlacement::Overlay(_))
                        && other.tab == tab
                        && other.placement == WindowPlacement::Fullscreen
                        && other.id != window.id
                })
        })
    }

    fn synchronize_focus_memory(&mut self) {
        let windows = &self.windows;
        self.owner_focus.retain(|(tab, owner), id| {
            windows
                .get(id)
                .is_some_and(|window| window.tab == *tab && window.owner == *owner)
        });
        if let Some(window) = self.focused.and_then(|id| windows.get(&id)) {
            self.owner_focus
                .insert((window.tab, window.owner.clone()), window.id);
        }
        let fullscreen = windows
            .values()
            .filter(|window| window.placement == WindowPlacement::Fullscreen)
            .map(|window| (window.tab, window.id))
            .collect::<BTreeMap<_, _>>();
        self.remembered_focus.retain(|tab, remembered| {
            windows.get(remembered).is_some_and(|window| {
                window.tab == *tab
                    && window.focusable
                    && window.placement != WindowPlacement::Hidden
                    && (fullscreen.get(tab).is_none_or(|id| id == remembered)
                        || matches!(window.placement, WindowPlacement::Overlay(_)))
            })
        });
        if let Some(focused) = self.focused
            && let Some(window) = self.windows.get(&focused)
            && window.focusable
            && window.placement != WindowPlacement::Hidden
        {
            self.remembered_focus.insert(window.tab, focused);
        }
    }

    pub(super) fn select_focus_fallback(&mut self) {
        if self.focused.is_some() {
            return;
        }
        let remembered = self.active_tab.and_then(|tab| {
            self.remembered_focus
                .get(&tab)
                .copied()
                .filter(|window| self.eligible_focus(*window, tab))
        });
        let candidate = remembered
            .and_then(|window| self.windows.get(&window))
            .or_else(|| {
                self.windows
                    .values()
                    .filter(|window| self.eligible_focus(window.id, window.tab))
                    .find(|window| self.active_tab == Some(window.tab))
            })
            .map(|window| window.id);
        if let Some(window) = candidate {
            self.focused = Some(window);
        }
    }
}

fn replace_tiled_leaf(
    node: &mut TiledLayoutNode,
    target: WindowId,
    replacement: TiledLayoutNode,
) -> bool {
    match node {
        TiledLayoutNode::Window(window) if *window == target => {
            *node = replacement;
            true
        }
        TiledLayoutNode::Window(_) => false,
        TiledLayoutNode::Split { first, second, .. } => {
            if contains_window(first, target) {
                replace_tiled_leaf(first, target, replacement)
            } else {
                replace_tiled_leaf(second, target, replacement)
            }
        }
    }
}
