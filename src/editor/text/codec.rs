//! Text encoding detection and file decoding.

use crate::editor::text::ByteOffset;
use crate::editor::text::PositionError;
use crate::editor::text::TextBuffer;

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum PositionEncoding {
    #[serde(rename = "utf-8")]
    Utf8,
    #[default]
    #[serde(rename = "utf-16")]
    Utf16,
    #[serde(rename = "utf-32")]
    Utf32,
}

impl PositionEncoding {
    #[must_use]
    pub const fn lsp_name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
            Self::Utf32 => "utf-32",
        }
    }

    #[must_use]
    pub fn parse_lsp(value: &str) -> Option<Self> {
        match value {
            "utf-8" => Some(Self::Utf8),
            "utf-16" => Some(Self::Utf16),
            "utf-32" => Some(Self::Utf32),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextPoint {
    pub line: usize,
    pub byte_column: usize,
}

pub struct PositionCodec<'a> {
    buffer: &'a TextBuffer,
}

impl<'a> PositionCodec<'a> {
    #[must_use]
    pub const fn new(buffer: &'a TextBuffer) -> Self {
        Self { buffer }
    }

    pub fn byte_to_point(&self, offset: ByteOffset) -> Result<TextPoint, PositionError> {
        let line = self.buffer.byte_to_line(offset)?;
        let line_start = self.buffer.line_to_byte(line)?;
        Ok(TextPoint {
            line,
            byte_column: offset.get() - line_start.get(),
        })
    }

    pub fn point_to_byte(&self, point: TextPoint) -> Result<ByteOffset, PositionError> {
        let line_start = self.buffer.line_to_byte(point.line)?;
        let offset = ByteOffset(line_start.get().saturating_add(point.byte_column));
        let next_line_start = if point.line + 1 < self.buffer.len_lines() {
            self.buffer.line_to_byte(point.line + 1)?.get()
        } else {
            self.buffer.len_bytes()
        };
        if offset.get() > next_line_start {
            return Err(PositionError::OutOfBounds {
                offset: offset.get(),
                len: next_line_start,
            });
        }
        self.buffer.validate_offset(offset)?;
        Ok(offset)
    }

    pub fn byte_to_lsp(
        &self,
        offset: ByteOffset,
        encoding: PositionEncoding,
    ) -> Result<(usize, usize), PositionError> {
        let point = self.byte_to_point(offset)?;
        let line = self.buffer.line(point.line)?;
        let prefix = line
            .get(..point.byte_column)
            .ok_or(PositionError::NotCharBoundary(point.byte_column))?;
        let character = match encoding {
            PositionEncoding::Utf8 => prefix.len(),
            PositionEncoding::Utf16 => prefix.encode_utf16().count(),
            PositionEncoding::Utf32 => prefix.chars().count(),
        };
        Ok((point.line, character))
    }

    pub fn lsp_to_byte(
        &self,
        line: usize,
        character: usize,
        encoding: PositionEncoding,
    ) -> Result<ByteOffset, PositionError> {
        // Protocol positions past the document or line end are clamped to that end.
        if line >= self.buffer.len_lines() {
            return Ok(ByteOffset(self.buffer.len_bytes()));
        }
        let text = self.buffer.line(line)?;
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let mut units = 0_usize;
        let mut byte_column = 0_usize;
        for scalar in content.chars() {
            if units == character {
                break;
            }
            let scalar_units = match encoding {
                PositionEncoding::Utf8 => scalar.len_utf8(),
                PositionEncoding::Utf16 => scalar.len_utf16(),
                PositionEncoding::Utf32 => 1,
            };
            let Some(next_units) = units.checked_add(scalar_units) else {
                return Err(PositionError::OutOfBounds {
                    offset: character,
                    len: units,
                });
            };
            if next_units > character {
                return Err(PositionError::InvalidProtocolColumn {
                    line,
                    column: character,
                    encoding: encoding.lsp_name(),
                });
            }
            units = next_units;
            byte_column += scalar.len_utf8();
        }
        self.point_to_byte(TextPoint { line, byte_column })
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::PositionCodec;
    use crate::editor::text::PositionEncoding;
    use crate::editor::text::TextBuffer;

    #[test]
    fn unicode_point_round_trip() {
        let buffer = TextBuffer::from_text("aé\n世界");
        let codec = PositionCodec::new(&buffer);
        for offset in [0, 1, 3, 4, 7, 10] {
            let offset = ByteOffset(offset);
            let point = codec.byte_to_point(offset).expect("boundary");
            assert_eq!(codec.point_to_byte(point).expect("round trip"), offset);
        }
    }

    #[test]
    fn lsp_positions_past_the_last_line_clamp_to_document_end() {
        for text in ["", "a😀é", "a😀é\n", "first\nlast"] {
            let buffer = TextBuffer::from_text(text);
            let codec = PositionCodec::new(&buffer);
            for encoding in [
                PositionEncoding::Utf8,
                PositionEncoding::Utf16,
                PositionEncoding::Utf32,
            ] {
                for line in [buffer.len_lines(), 999, usize::MAX] {
                    for character in [0, 1, usize::MAX] {
                        assert_eq!(
                            codec.lsp_to_byte(line, character, encoding).unwrap(),
                            ByteOffset(text.len()),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lsp_position_round_trips_all_negotiated_encodings() {
        let buffer = TextBuffer::from_text("a😀é\n界z");
        let codec = PositionCodec::new(&buffer);
        for encoding in [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ] {
            for offset in [0, 1, 5, 7, 8, 11, 12] {
                let offset = ByteOffset(offset);
                let (line, character) = codec.byte_to_lsp(offset, encoding).expect("encode");
                assert_eq!(
                    codec
                        .lsp_to_byte(line, character, encoding)
                        .expect("decode"),
                    offset
                );
            }
        }
        assert!(codec.lsp_to_byte(0, 2, PositionEncoding::Utf16).is_err());
        assert_eq!(
            codec
                .lsp_to_byte(0, 99, PositionEncoding::Utf16)
                .expect("past the end"),
            ByteOffset(7)
        );
    }
}
