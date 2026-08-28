use std::fmt;

use ropey::Rope;

use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, DecodedText, EncodingError, FileFormat, PositionError,
    decode_utf8, encode_utf8,
};

/// Rope-backed normalized UTF-8 text and its file-format metadata.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    pub(crate) rope: Rope,
    revision: BufferRevision,
    format: FileFormat,
}

impl TextBuffer {
    #[must_use]
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
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EncodingError> {
        let DecodedText { text, format } = decode_utf8(bytes)?;
        Ok(Self {
            rope: Rope::from_str(&text),
            revision: BufferRevision::initial(),
            format,
        })
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
        Ok(())
    }

    /// Restore a historical snapshot while preserving monotonic revisions.
    pub fn restore_text(&mut self, text: &str) -> Result<(), PositionError> {
        self.rope = Rope::from_str(text);
        self.bump_revision()
    }
}

impl fmt::Display for TextBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
