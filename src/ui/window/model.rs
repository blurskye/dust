use super::*;

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
