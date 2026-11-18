//! Byte offsets, ranges and line positions.

use std::fmt;

use thiserror::Error;

/// A UTF-8 byte offset in a specific buffer revision.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(pub usize);

impl ByteOffset {
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for ByteOffset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A half-open UTF-8 byte range.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ByteRange {
    pub start: ByteOffset,
    pub end: ByteOffset,
}

impl ByteRange {
    pub fn new(start: ByteOffset, end: ByteOffset) -> Result<Self, PositionError> {
        if start > end {
            return Err(PositionError::ReversedRange {
                start: start.get(),
                end: end.get(),
            });
        }
        Ok(Self { start, end })
    }

    #[must_use]
    pub const fn empty(at: ByteOffset) -> Self {
        Self { start: at, end: at }
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end.0 - self.start.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    #[must_use]
    pub const fn contains(self, offset: ByteOffset) -> bool {
        self.start.0 <= offset.0 && offset.0 < self.end.0
    }
}

/// Monotonically increasing text revision.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BufferRevision(pub u64);

impl BufferRevision {
    #[must_use]
    pub const fn initial() -> Self {
        Self(0)
    }

    pub fn next(self) -> Result<Self, PositionError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(PositionError::RevisionOverflow)
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum PositionError {
    #[error("range starts at {start} after its end at {end}")]
    ReversedRange { start: usize, end: usize },
    #[error("byte offset {offset} is outside a text of {len} bytes")]
    OutOfBounds { offset: usize, len: usize },
    #[error("byte offset {0} is not a UTF-8 character boundary")]
    NotCharBoundary(usize),
    #[error("line {line} is outside a text with {lines} lines")]
    LineOutOfBounds { line: usize, lines: usize },
    #[error("protocol column {column} on line {line} splits a scalar in {encoding} encoding")]
    InvalidProtocolColumn {
        line: usize,
        column: usize,
        encoding: &'static str,
    },
    #[error("buffer revision overflow")]
    RevisionOverflow,
}
