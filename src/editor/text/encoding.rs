//! Text encoding names and conversion policy.

use thiserror::Error;

/// A line separator observed in the source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

/// Encoding information required to faithfully write a UTF-8 file back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileFormat {
    pub utf8_bom: bool,
    /// One entry for every normalized newline in the decoded source.
    pub line_endings: Vec<LineEnding>,
    pub preferred_line_ending: LineEnding,
    pub had_final_newline: bool,
}

impl Default for FileFormat {
    fn default() -> Self {
        Self {
            utf8_bom: false,
            line_endings: Vec::new(),
            preferred_line_ending: LineEnding::Lf,
            had_final_newline: false,
        }
    }
}

impl FileFormat {
    #[must_use]
    pub fn is_mixed(&self) -> bool {
        self.line_endings
            .first()
            .is_some_and(|first| self.line_endings.iter().any(|ending| ending != first))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedText {
    pub text: String,
    pub format: FileFormat,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum EncodingError {
    #[error("file is not valid UTF-8 (first invalid byte near offset {valid_up_to})")]
    InvalidUtf8 { valid_up_to: usize },
}

/// Decode UTF-8, normalize all recognized line separators to `\n`, and retain
/// enough metadata to reproduce the separators that still exist after edits.
pub fn decode_utf8(bytes: &[u8]) -> Result<DecodedText, EncodingError> {
    let (utf8_bom, payload) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(payload) => (true, payload),
        None => (false, bytes),
    };
    let source = std::str::from_utf8(payload).map_err(|error| EncodingError::InvalidUtf8 {
        valid_up_to: bytes.len() - payload.len() + error.valid_up_to(),
    })?;

    let mut text = String::with_capacity(source.len());
    let mut endings = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                text.push('\n');
                endings.push(LineEnding::CrLf);
            }
            '\r' => {
                text.push('\n');
                endings.push(LineEnding::Cr);
            }
            '\n' => {
                text.push('\n');
                endings.push(LineEnding::Lf);
            }
            other => text.push(other),
        }
    }

    let preferred_line_ending = preferred_ending(&endings);
    let had_final_newline = text.ends_with('\n');
    Ok(DecodedText {
        text,
        format: FileFormat {
            utf8_bom,
            line_endings: endings,
            preferred_line_ending,
            had_final_newline,
        },
    })
}

fn preferred_ending(endings: &[LineEnding]) -> LineEnding {
    let counts = endings.iter().fold([0_usize; 3], |mut counts, ending| {
        let index = match ending {
            LineEnding::Lf => 0,
            LineEnding::CrLf => 1,
            LineEnding::Cr => 2,
        };
        counts[index] += 1;
        counts
    });
    match counts
        .iter()
        .enumerate()
        .max_by_key(|(index, count)| (**count, std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
    {
        Some(1) => LineEnding::CrLf,
        Some(2) => LineEnding::Cr,
        _ => LineEnding::Lf,
    }
}

/// Encode normalized UTF-8 using the original line ending at each surviving
/// newline. Newly inserted lines use the source's preferred separator.
#[must_use]
pub fn encode_utf8(text: &str, format: &FileFormat) -> Vec<u8> {
    let newline_count = text.bytes().filter(|byte| *byte == b'\n').count();
    let mut output =
        Vec::with_capacity(text.len() + newline_count + usize::from(format.utf8_bom) * 3);
    if format.utf8_bom {
        output.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    let mut ending_index = 0;
    for segment in text.split_inclusive('\n') {
        if let Some(content) = segment.strip_suffix('\n') {
            output.extend_from_slice(content.as_bytes());
            let ending = format
                .line_endings
                .get(ending_index)
                .copied()
                .unwrap_or(format.preferred_line_ending);
            output.extend_from_slice(ending.as_str().as_bytes());
            ending_index += 1;
        } else {
            output.extend_from_slice(segment.as_bytes());
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::editor::text::encoding::LineEnding;
    use crate::editor::text::encoding::decode_utf8;
    use crate::editor::text::encoding::encode_utf8;

    #[test]
    fn preserves_bom_mixed_endings_and_final_newline() {
        let input = b"\xEF\xBB\xBFone\r\ntwo\nthree\r";
        let decoded = decode_utf8(input).expect("valid UTF-8");
        assert_eq!(decoded.text, "one\ntwo\nthree\n");
        assert!(decoded.format.utf8_bom);
        assert!(decoded.format.is_mixed());
        assert!(decoded.format.had_final_newline);
        assert_eq!(
            decoded.format.line_endings,
            vec![LineEnding::CrLf, LineEnding::Lf, LineEnding::Cr]
        );
        assert_eq!(encode_utf8(&decoded.text, &decoded.format), input);
    }

    #[test]
    fn invalid_utf8_offsets_include_the_bom() {
        for input in [b"\xEF\xBB\xBF\xFF".as_slice(), b"\xEF\xBB\xBFabc\xFF"] {
            assert_eq!(
                decode_utf8(input),
                Err(crate::editor::text::encoding::EncodingError::InvalidUtf8 {
                    valid_up_to: input.len() - 1,
                })
            );
        }
    }

    #[test]
    fn rejects_invalid_utf8_without_lossy_conversion() {
        let error = decode_utf8(&[b'a', 0xFF]).expect_err("invalid bytes must fail");
        assert_eq!(
            error.to_string(),
            "file is not valid UTF-8 (first invalid byte near offset 1)"
        );
    }
}
