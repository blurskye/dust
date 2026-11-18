//! Content, view, window and workspace-tab identifiers.

use crate::windows::MAXIMUM_CONTENT_TYPE_ID_BYTES;
use crate::windows::error::WindowError;

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
