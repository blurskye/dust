use std::fmt;

use ropey::Rope;

use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, DecodedText, EncodingError, FileFormat, PositionError,
    decode_utf8, encode_utf8,
};

/// Rope-backed normalized UTF-8 text and its file-format metadata.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    pub(in crate::editor::text) rope: Rope,
    revision: BufferRevision,
    format: FileFormat,
    source: TextVersion,
}

/// Exact immutable text snapshot identity, including reloads at equal revisions.
#[derive(Clone, Debug)]
pub(crate) struct TextVersion {
    origin: std::sync::Arc<()>,
    snapshot: std::sync::Arc<()>,
}
impl TextVersion {
    fn new() -> Self {
        Self {
            origin: std::sync::Arc::new(()),
            snapshot: std::sync::Arc::new(()),
        }
    }
    pub(crate) fn same_incarnation(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.origin, &other.origin)
    }
}
impl PartialEq for TextVersion {
    fn eq(&self, other: &Self) -> bool {
        self.same_incarnation(other) && std::sync::Arc::ptr_eq(&self.snapshot, &other.snapshot)
    }
}
impl Eq for TextVersion {}

#[cfg(test)]
thread_local! { static TEXT_MATERIALIZATIONS: std::cell::Cell<(usize,usize)> = const { std::cell::Cell::new((0,0)) }; }
impl TextBuffer {
    #[cfg(test)]
    pub(crate) fn test_materializations() -> (usize, usize) {
        TEXT_MATERIALIZATIONS.with(std::cell::Cell::get)
    }

    #[must_use]
    pub(crate) fn matches_encoded_text(&self, bytes: &[u8]) -> bool {
        let mut bytes = bytes
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(bytes)
            .iter()
            .copied()
            .peekable();
        let normalized = std::iter::from_fn(|| {
            Some(match bytes.next()? {
                b'\r' => {
                    if bytes.peek() == Some(&b'\n') {
                        bytes.next();
                    }
                    b'\n'
                }
                byte => byte,
            })
        });
        self.rope.chunks().flat_map(str::bytes).eq(normalized)
    }
    pub fn from_text(text: &str) -> Self {
        let normalized = normalize_newlines(text);
        let newline_count = normalized.bytes().filter(|byte| *byte == b'\n').count();
        let format = FileFormat {
            line_endings: vec![crate::editor::text::LineEnding::Lf; newline_count],
            had_final_newline: normalized.ends_with('\n'),
            ..FileFormat::default()
        };
        Self {
            rope: Rope::from_str(&normalized),
            revision: BufferRevision::initial(),
            format,
            source: TextVersion::new(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EncodingError> {
        #[cfg(test)]
        TEXT_MATERIALIZATIONS.with(|n| {
            let (decode, display) = n.get();
            n.set((decode + 1, display));
        });
        let DecodedText { text, format } = decode_utf8(bytes)?;
        Ok(Self {
            rope: Rope::from_str(&text),
            revision: BufferRevision::initial(),
            format,
            source: TextVersion::new(),
        })
    }

    pub(crate) fn source_version(&self) -> TextVersion {
        self.source.clone()
    }

    #[must_use]
    pub const fn revision(&self) -> BufferRevision {
        self.revision
    }

    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        encode_utf8(&self.to_string(), &self.format)
    }

    #[must_use]
    pub fn format(&self) -> &FileFormat {
        &self.format
    }

    pub fn validate_offset(&self, offset: ByteOffset) -> Result<(), PositionError> {
        if offset.get() > self.len_bytes() {
            return Err(PositionError::OutOfBounds {
                offset: offset.get(),
                len: self.len_bytes(),
            });
        }
        if !self.is_char_boundary(offset) {
            return Err(PositionError::NotCharBoundary(offset.get()));
        }
        Ok(())
    }

    pub fn validate_range(&self, range: ByteRange) -> Result<(), PositionError> {
        self.validate_offset(range.start)?;
        self.validate_offset(range.end)
    }

    #[must_use]
    pub fn is_char_boundary(&self, offset: ByteOffset) -> bool {
        if offset.get() > self.len_bytes() {
            return false;
        }
        if offset.get() == 0 || offset.get() == self.len_bytes() {
            return true;
        }
        self.rope
            .get_byte(offset.get())
            .is_some_and(|byte| byte & 0b1100_0000 != 0b1000_0000)
    }

    pub fn byte_to_char(&self, offset: ByteOffset) -> Result<usize, PositionError> {
        self.validate_offset(offset)?;
        Ok(self.rope.byte_to_char(offset.get()))
    }

    pub fn char_to_byte(&self, char_offset: usize) -> Result<ByteOffset, PositionError> {
        if char_offset > self.len_chars() {
            return Err(PositionError::OutOfBounds {
                offset: char_offset,
                len: self.len_chars(),
            });
        }
        Ok(ByteOffset(self.rope.char_to_byte(char_offset)))
    }

    pub fn byte_to_line(&self, offset: ByteOffset) -> Result<usize, PositionError> {
        self.validate_offset(offset)?;
        Ok(self.rope.byte_to_line(offset.get()))
    }

    pub fn line_to_byte(&self, line: usize) -> Result<ByteOffset, PositionError> {
        if line >= self.len_lines() {
            return Err(PositionError::LineOutOfBounds {
                line,
                lines: self.len_lines(),
            });
        }
        Ok(ByteOffset(self.rope.line_to_byte(line)))
    }

    pub fn line(&self, line: usize) -> Result<String, PositionError> {
        if line >= self.len_lines() {
            return Err(PositionError::LineOutOfBounds {
                line,
                lines: self.len_lines(),
            });
        }
        Ok(self.rope.line(line).to_string())
    }

    pub fn slice(&self, range: ByteRange) -> Result<String, PositionError> {
        self.validate_range(range)?;
        let start = self.byte_to_char(range.start)?;
        let end = self.byte_to_char(range.end)?;
        Ok(self.rope.slice(start..end).to_string())
    }

    pub(crate) fn bump_revision(&mut self) -> Result<(), PositionError> {
        self.revision = self.revision.next()?;
        self.source.snapshot = std::sync::Arc::new(());
        Ok(())
    }

    /// Restore a historical snapshot while preserving monotonic revisions.
    pub fn restore_text(&mut self, text: &str) -> Result<(), PositionError> {
        self.bump_revision()?;
        self.rope = Rope::from_str(text);
        Ok(())
    }
}

impl fmt::Display for TextBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(test)]
        TEXT_MATERIALIZATIONS.with(|n| {
            let (decode, display) = n.get();
            n.set((decode, display + 1));
        });
        for chunk in self.rope.chunks() {
            formatter.write_str(chunk)?;
        }
        Ok(())
    }
}

impl PartialEq for TextBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.format == other.format
            && self.to_string() == other.to_string()
    }
}

fn normalize_newlines(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' if characters.peek() == Some(&'\n') => {
                characters.next();
                normalized.push('\n');
            }
            '\r' => normalized.push('\n'),
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod source_tests {
    use super::*;
    #[test]
    fn exact_snapshots_distinguish_branches_and_reload_but_keep_edit_incarnation() {
        let original = TextBuffer::from_text("before");
        let mut a = original.clone();
        let mut b = original.clone();
        a.restore_text("branch a").unwrap();
        b.restore_text("branch b").unwrap();
        assert_eq!(a.revision(), b.revision());
        assert_ne!(a.source_version(), b.source_version());
        assert!(a.source_version().same_incarnation(&b.source_version()));
        assert!(
            !a.source_version()
                .same_incarnation(&TextBuffer::from_text("branch a").source_version())
        );
        a.revision = BufferRevision(u64::MAX);
        let source = a.source_version();
        let bytes = a.to_string();
        assert!(a.restore_text("must not publish").is_err());
        assert_eq!(a.to_string(), bytes);
        assert_eq!(a.source_version(), source);
    }
}
