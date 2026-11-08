//! Window validation failures and checks for resource identities and titles.

use crate::windows::MAXIMUM_OWNER_BYTES;
use crate::windows::MAXIMUM_TITLE_BYTES;
use thiserror::Error;

pub(super) fn increment(value: &mut u64) -> Result<u64, WindowError> {
    *value = value
        .checked_add(1)
        .ok_or(WindowError::IdentifierExhausted)?;
    Ok(*value)
}

pub(super) fn validate_identity(value: u64) -> Result<(), WindowError> {
    (value != 0)
        .then_some(())
        .ok_or(WindowError::InvalidIdentity)
}

pub(super) fn validate_owner(owner: &str, instance_id: u64) -> Result<(), WindowError> {
    (!owner.is_empty() && owner.len() <= MAXIMUM_OWNER_BYTES && instance_id != 0)
        .then_some(())
        .ok_or(WindowError::InvalidOwner)
}

pub(crate) fn validate_title(title: &str) -> Result<(), WindowError> {
    (title.len() <= MAXIMUM_TITLE_BYTES && !title.chars().any(char::is_control))
        .then_some(())
        .ok_or(WindowError::InvalidTitle)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WindowError {
    #[error("invalid content type `{0}`")]
    InvalidContentType(String),
    #[error("invalid zero window identity")]
    InvalidIdentity,
    #[error("invalid window resource owner")]
    InvalidOwner,
    #[error("invalid window content title")]
    InvalidTitle,
    #[error("invalid window placement")]
    InvalidPlacement,
    #[error("content does not support {0}")]
    Unsupported(&'static str),
    #[error("duplicate content identity")]
    DuplicateContent,
    #[error("unknown content identity")]
    UnknownContent,
    #[error("unknown view identity")]
    UnknownView,
    #[error("unknown window identity")]
    UnknownWindow,
    #[error("unknown workspace-tab identity")]
    UnknownWorkspaceTab,
    #[error("window target is outside the active workspace tab")]
    WindowOutsideActiveTab,
    #[error("window content target resolves to multiple windows")]
    AmbiguousWindowTarget,
    #[error("view is already hosted by a window")]
    ViewAlreadyHosted,
    #[error("view is still hosted by a window")]
    ViewStillHosted,
    #[error("content still owns a view")]
    ContentStillViewed,
    #[error("workspace tab still owns a window")]
    WorkspaceTabStillHosted,
    #[error("focused window is outside the active workspace tab")]
    FocusOutsideActiveTab,
    #[error("window is not focusable")]
    NotFocusable,
    #[error("workspace tab has no tiled layout")]
    MissingTiledLayout,
    #[error("window is absent from the tiled layout")]
    WindowNotInTiledLayout,
    #[error("tiled layout references a stale window")]
    StaleTiledWindow,
    #[error("tiled layout references a non-tiled or foreign window")]
    NonTiledWindowInLayout,
    #[error("tiled layout references one window more than once")]
    DuplicateTiledWindow,
    #[error("tiled window is absent from its workspace-tab layout")]
    OrphanTiledWindow,
    #[error("tiled layout exceeds its depth limit")]
    TiledLayoutTooDeep,
    #[error("tiled layout exceeds its window limit")]
    TooManyTiledWindows,
    #[error("workspace tab exceeds its non-tiled window limit")]
    TooManyNonTiledWindows,
    #[error("workspace tab exceeds its overlay window limit")]
    TooManyOverlayWindows,
    #[error("workspace tab owns more than one fullscreen window")]
    MultipleFullscreenWindows,
    #[error("editor tiled mutation is not yet graph-authoritative")]
    TiledMutationNotAuthoritative,
    #[error("window graph revision space is exhausted")]
    RevisionExhausted,
    #[error("window identifier space exhausted")]
    IdentifierExhausted,
}
