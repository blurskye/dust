use super::*;

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

pub(super) fn validate_owner(owner: &str, generation: u64) -> Result<(), WindowError> {
    (!owner.is_empty() && owner.len() <= MAXIMUM_OWNER_BYTES && generation != 0)
        .then_some(())
        .ok_or(WindowError::InvalidOwner)
}

pub(crate) fn validate_title(title: &str) -> Result<(), WindowError> {
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
