//! Content capabilities, lifecycle and the instances that own graph resources.

use crate::windows::identity::ContentId;
use crate::windows::identity::ContentTypeId;
use crate::windows::identity::ViewId;
use crate::windows::identity::WindowId;
use crate::windows::identity::WorkspaceTabId;
use crate::windows::layout::ContentAdapterKind;
use crate::windows::layout::TiledLayoutNode;
use crate::windows::layout::WindowPlacement;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentLifecycle {
    Ready,
    Preflight,
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentContract {
    pub adapter: ContentAdapterKind,
    pub capabilities: ContentCapabilities,
    pub lifecycle: ContentLifecycle,
}

impl ContentContract {
    #[must_use]
    pub const fn adapter_defaults(adapter: ContentAdapterKind) -> Self {
        Self {
            adapter,
            capabilities: ContentCapabilities::for_adapter(adapter),
            lifecycle: match adapter {
                ContentAdapterKind::TerminalSession | ContentAdapterKind::ExternalTui => {
                    ContentLifecycle::Starting
                }
                _ => ContentLifecycle::Ready,
            },
        }
    }

    #[must_use]
    pub const fn declared(adapter: ContentAdapterKind, capabilities: ContentCapabilities) -> Self {
        let mut contract = Self::adapter_defaults(adapter);
        contract.capabilities = capabilities;
        contract
    }

    #[must_use]
    pub const fn with_lifecycle(mut self, lifecycle: ContentLifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentCapabilities {
    pub focusable: bool,
    pub selectable: bool,
    pub editable: bool,
    pub clone_view: bool,
    pub tile: bool,
    pub dock: bool,
    pub float: bool,
    pub fullscreen: bool,
    pub hide: bool,
    pub close_view: bool,
    pub stop_resource: bool,
    pub persist_descriptor: bool,
}

impl ContentCapabilities {
    #[must_use]
    pub const fn for_adapter(adapter: ContentAdapterKind) -> Self {
        match adapter {
            ContentAdapterKind::TextBuffer => Self {
                focusable: true,
                selectable: true,
                editable: true,
                clone_view: true,
                tile: true,
                dock: true,
                float: true,
                fullscreen: true,
                hide: true,
                close_view: true,
                stop_resource: false,
                persist_descriptor: true,
            },
            ContentAdapterKind::TerminalSession | ContentAdapterKind::ExternalTui => Self {
                focusable: true,
                selectable: true,
                editable: false,
                clone_view: false,
                tile: true,
                dock: true,
                float: true,
                fullscreen: true,
                hide: true,
                close_view: true,
                stop_resource: true,
                persist_descriptor: false,
            },
            ContentAdapterKind::VirtualDocument => Self {
                focusable: true,
                selectable: true,
                editable: false,
                clone_view: true,
                tile: true,
                dock: true,
                float: true,
                fullscreen: true,
                hide: true,
                close_view: true,
                stop_resource: false,
                persist_descriptor: true,
            },
            ContentAdapterKind::DeclarativeUi | ContentAdapterKind::PrivateCellSurface => Self {
                focusable: true,
                selectable: true,
                editable: false,
                clone_view: false,
                tile: true,
                dock: true,
                float: true,
                fullscreen: true,
                hide: true,
                close_view: true,
                stop_resource: false,
                persist_descriptor: false,
            },
        }
    }

    pub(super) const fn permits(self, placement: WindowPlacement) -> bool {
        match placement {
            WindowPlacement::Tiled => self.tile,
            WindowPlacement::Docked { .. } => self.dock,
            WindowPlacement::Floating(_) => self.float,
            WindowPlacement::Fullscreen => self.fullscreen,
            WindowPlacement::Hidden => self.hide,
            WindowPlacement::Overlay(_) => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentDescriptor {
    pub id: ContentId,
    pub content_type: ContentTypeId,
    pub adapter: ContentAdapterKind,
    pub capabilities: ContentCapabilities,
    pub lifecycle: ContentLifecycle,
    pub title: String,
    pub dirty: bool,
    pub owner: String,
    pub instance_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewDescriptor {
    pub id: ViewId,
    pub content: ContentId,
    pub owner: String,
    pub instance_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTabDescriptor {
    pub id: WorkspaceTabId,
    pub tiled_root: Option<TiledLayoutNode>,
    pub owner: String,
    pub instance_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDescriptor {
    pub id: WindowId,
    pub tab: WorkspaceTabId,
    pub view: ViewId,
    pub placement: WindowPlacement,
    pub restore_placement: WindowPlacement,
    pub focusable: bool,
    pub owner: String,
    pub instance_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentSnapshot {
    pub id: ContentId,
    pub content_type: ContentTypeId,
    pub adapter: ContentAdapterKind,
    pub capabilities: ContentCapabilities,
    pub lifecycle: ContentLifecycle,
    pub title: String,
    pub dirty: bool,
    pub active: bool,
    pub view_count: usize,
    pub window_count: usize,
}
