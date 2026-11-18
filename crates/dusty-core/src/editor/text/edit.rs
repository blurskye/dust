//! Byte-range edits and checked edit sets.

use thiserror::Error;

use crate::editor::text::BufferRevision;

use crate::editor::text::ByteRange;

use crate::editor::text::DeltaSpan;

use crate::editor::text::EditDelta;

use crate::editor::text::PositionError;

use crate::editor::text::TextBuffer;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    range: ByteRange,
    replacement: String,
}

impl TextEdit {
    #[must_use]
    pub fn new(range: ByteRange, replacement: impl Into<String>) -> Self {
        Self {
            range,
            replacement: normalize_replacement(replacement.into()),
        }
    }

    #[must_use]
    pub const fn range(&self) -> ByteRange {
        self.range
    }

    #[must_use]
    pub fn replacement(&self) -> &str {
        &self.replacement
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEditSet {
    base_revision: BufferRevision,
    edits: Vec<TextEdit>,
    normalize_line_endings: bool,
}

impl TextEditSet {
    #[must_use]
    pub fn new(base_revision: BufferRevision, edits: Vec<TextEdit>) -> Self {
        Self {
            base_revision,
            edits,
            normalize_line_endings: false,
        }
    }

    #[must_use]
    pub fn with_normalized_line_endings(mut self) -> Self {
        self.normalize_line_endings = true;
        self
    }

    #[must_use]
    pub const fn base_revision(&self) -> BufferRevision {
        self.base_revision
    }

    #[must_use]
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum EditError {
    #[error("edit targets revision {actual:?}, but buffer is at {expected:?}")]
    StaleRevision {
        expected: BufferRevision,
        actual: BufferRevision,
    },
    #[error(transparent)]
    InvalidPosition(#[from] PositionError),
    #[error(
        "edits overlap at byte range {first_start}..{first_end} and {second_start}..{second_end}"
    )]
    OverlappingEdits {
        first_start: usize,
        first_end: usize,
        second_start: usize,
        second_end: usize,
    },
}

impl TextBuffer {
    pub fn validate_edits(&self, set: &TextEditSet) -> Result<Vec<TextEdit>, EditError> {
        if set.base_revision != self.revision() {
            return Err(EditError::StaleRevision {
                expected: self.revision(),
                actual: set.base_revision,
            });
        }
        let mut sorted = set.edits.clone();
        // Stable sorting retains the supplied order of insertions at the same position.
        sorted.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut edits: Vec<TextEdit> = Vec::with_capacity(sorted.len());
        for edit in sorted {
            self.validate_range(edit.range)?;
            if let Some(previous) = edits.last_mut()
                && previous.range.is_empty()
                && previous.range == edit.range
            {
                previous.replacement.push_str(&edit.replacement);
            } else {
                edits.push(edit);
            }
        }
        for pair in edits.windows(2) {
            let first = &pair[0];
            let second = &pair[1];
            if second.range.start < first.range.end {
                return Err(EditError::OverlappingEdits {
                    first_start: first.range.start.get(),
                    first_end: first.range.end.get(),
                    second_start: second.range.start.get(),
                    second_end: second.range.end.get(),
                });
            }
        }
        edits.retain(|edit| {
            !self
                .rope
                .byte_slice(edit.range.start.get()..edit.range.end.get())
                .bytes()
                .eq(edit.replacement.bytes())
                || (set.normalize_line_endings
                    && self
                        .line_endings(edit.range)
                        .iter()
                        .any(|ending| *ending != self.format.preferred_line_ending))
        });
        Ok(edits)
    }

    /// Validate the complete edit set before changing the rope, then apply from
    /// the end of the document toward the beginning.
    pub fn apply_edits(&mut self, set: &TextEditSet) -> Result<EditDelta, EditError> {
        let edits = self.validate_edits(set)?;
        let delta = EditDelta::new(
            edits
                .iter()
                .map(|edit| DeltaSpan {
                    old: edit.range,
                    inserted_len: edit.replacement.len(),
                })
                .collect(),
        );
        let previous = self.rope.clone();
        self.format.line_endings.resize(
            self.len_lines().saturating_sub(1),
            self.format.preferred_line_ending,
        );
        for edit in edits.iter().rev() {
            let start = self.byte_to_char(edit.range.start)?;
            let end = self.byte_to_char(edit.range.end)?;
            let first_line = self.byte_to_line(edit.range.start)?;
            let last_line = self.byte_to_line(edit.range.end)?;
            let inserted = edit
                .replacement
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            self.format.line_endings.splice(
                first_line..last_line,
                std::iter::repeat_n(self.format.preferred_line_ending, inserted),
            );
            self.rope.remove(start..end);
            self.rope.insert(start, &edit.replacement);
        }
        if !edits.is_empty() {
            self.format.had_final_newline = self
                .rope
                .len_chars()
                .checked_sub(1)
                .is_some_and(|last| self.rope.char(last) == '\n');
            let before = self.revision();
            self.bump_revision()?;
            self.remember_edit(before, delta.clone(), previous);
        }
        Ok(delta)
    }
}

fn normalize_replacement(text: String) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use crate::editor::text::BufferRevision;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::editor::text::TextEdit;
    use crate::editor::text::TextEditSet;

    #[test]
    fn applies_unsorted_edits_atomically() {
        let mut buffer = TextBuffer::from_text("abcdef");
        let set = TextEditSet::new(
            BufferRevision::initial(),
            vec![
                TextEdit::new(
                    ByteRange::new(ByteOffset(4), ByteOffset(6)).expect("range"),
                    "Z",
                ),
                TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(2)).expect("range"),
                    "X",
                ),
            ],
        );
        buffer.apply_edits(&set).expect("valid edits");
        assert_eq!(buffer.to_string(), "XcdZ");
        assert_eq!(buffer.revision().0, 1);
    }

    #[test]
    fn coalesces_same_position_insertions_in_supplied_order() {
        let mut buffer = TextBuffer::from_text("ab");
        let set = TextEditSet::new(
            buffer.revision(),
            vec![
                TextEdit::new(ByteRange::empty(ByteOffset(1)), "first"),
                TextEdit::new(ByteRange::empty(ByteOffset(0)), "prefix"),
                TextEdit::new(ByteRange::empty(ByteOffset(1)), "second"),
                TextEdit::new(ByteRange::empty(ByteOffset(1)), "third"),
                TextEdit::new(ByteRange::new(ByteOffset(1), ByteOffset(2)).unwrap(), "B"),
            ],
        );
        let edits = buffer.validate_edits(&set).unwrap();
        assert_eq!(edits.len(), 3);
        assert_eq!(edits[1].replacement(), "firstsecondthird");
        let delta = buffer.apply_edits(&set).unwrap();
        assert_eq!(buffer.to_string(), "prefixafirstsecondthirdB");
        assert_eq!(delta.spans().len(), 3);
    }

    #[test]
    fn unchanged_replacements_preserve_mixed_line_endings_and_revision() {
        let original = b"a\r\nb\nc\n";
        let mut buffer = TextBuffer::from_bytes(original).unwrap();
        let revision = buffer.revision();
        let source = buffer.source_version();
        let unchanged = TextEdit::new(ByteRange::new(ByteOffset(0), ByteOffset(2)).unwrap(), "a\n");
        let delta = buffer
            .apply_edits(&TextEditSet::new(revision, vec![unchanged.clone()]))
            .unwrap();
        assert!(delta.spans().is_empty());
        assert_eq!(buffer.to_bytes(), original);
        assert_eq!(buffer.revision(), revision);
        assert_eq!(buffer.source_version(), source);

        let delta = buffer
            .apply_edits(&TextEditSet::new(
                revision,
                vec![
                    unchanged,
                    TextEdit::new(ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap(), "B"),
                ],
            ))
            .unwrap();
        assert_eq!(buffer.to_bytes(), b"a\r\nB\nc\n");
        assert_eq!(delta.spans().len(), 1);
    }

    #[test]
    fn overlap_failure_does_not_mutate() {
        let mut buffer = TextBuffer::from_text("abcdef");
        let original = buffer.clone();
        let set = TextEditSet::new(
            buffer.revision(),
            vec![
                TextEdit::new(
                    ByteRange::new(ByteOffset(1), ByteOffset(4)).expect("range"),
                    "",
                ),
                TextEdit::new(
                    ByteRange::new(ByteOffset(3), ByteOffset(5)).expect("range"),
                    "",
                ),
            ],
        );
        assert!(buffer.apply_edits(&set).is_err());
        assert_eq!(buffer, original);
    }

    #[test]
    fn rejects_mid_codepoint_boundary() {
        let mut buffer = TextBuffer::from_text("aéz");
        let set = TextEditSet::new(
            buffer.revision(),
            vec![TextEdit::new(
                ByteRange::new(ByteOffset(2), ByteOffset(3)).expect("ordered"),
                "x",
            )],
        );
        assert!(buffer.apply_edits(&set).is_err());
        assert_eq!(buffer.to_string(), "aéz");
    }
}
