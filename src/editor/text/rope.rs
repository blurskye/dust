//! Rope-backed text buffers and revisions.

use std::fmt;

use ropey::Rope;

use crate::editor::text::Affinity;

use crate::editor::text::BufferRevision;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::DecodedText;

use crate::editor::text::EditDelta;

use crate::editor::text::EncodingError;

use crate::editor::text::FileFormat;

use crate::editor::text::PositionError;

use crate::editor::text::decode_utf8;

use crate::editor::text::encode_utf8;

/// How many recent edits a buffer remembers for moving highlights forward.
const RECENT_EDITS: usize = 64;
/// How many texts before recent edits a buffer keeps, so a language server's late report can be
/// read against the text it was made for. Ropes share their unchanged parts, but a file over
/// the size limit keeps none, so one big replace can't hold several copies of a huge file.
const RECENT_TEXTS: usize = 8;
const RECENT_TEXT_BYTES: usize = 4 << 20;

/// Rope-backed normalized UTF-8 text and its file-format metadata.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    pub(in crate::editor::text) rope: Rope,
    revision: BufferRevision,
    pub(in crate::editor::text) format: FileFormat,
    source: TextVersion,
    /// The latest edits, each with the revision it was applied to, so highlights computed for
    /// an older revision can be moved onto the current text until a new parse arrives.
    recent_edits: Vec<(BufferRevision, EditDelta)>,
    recent_texts: Vec<(BufferRevision, Rope)>,
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
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.origin, &other.origin)
    }
}
impl PartialEq for TextVersion {
    fn eq(&self, other: &Self) -> bool {
        self.same_instance(other) && std::sync::Arc::ptr_eq(&self.snapshot, &other.snapshot)
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

    /// Compare file bytes without allocating a second encoded copy of the document.
    #[must_use]
    pub(crate) fn matches_encoded_text(&self, bytes: &[u8]) -> bool {
        let mut expected = bytes.iter().copied();
        if self.format.utf8_bom
            && ![0xef, 0xbb, 0xbf]
                .into_iter()
                .all(|byte| expected.next() == Some(byte))
        {
            return false;
        }
        let mut endings = self.format.line_endings.iter();
        for byte in self.rope.chunks().flat_map(str::bytes) {
            if byte == b'\n' {
                let ending = endings
                    .next()
                    .copied()
                    .unwrap_or(self.format.preferred_line_ending);
                if !ending
                    .as_str()
                    .bytes()
                    .all(|byte| expected.next() == Some(byte))
                {
                    return false;
                }
            } else if expected.next() != Some(byte) {
                return false;
            }
        }
        expected.next().is_none()
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
            recent_edits: Vec::new(),
            recent_texts: Vec::new(),
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
            recent_edits: Vec::new(),
            recent_texts: Vec::new(),
        })
    }

    pub(crate) fn source_version(&self) -> TextVersion {
        self.source.clone()
    }

    #[must_use]
    pub const fn revision(&self) -> BufferRevision {
        self.revision
    }

    pub(crate) fn set_preferred_line_ending(&mut self, ending: crate::editor::text::LineEnding) {
        self.format.preferred_line_ending = ending;
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

    /// Lines as Vim counts them: a final newline ends the last line rather than starting an
    /// empty one.
    #[must_use]
    pub fn visible_lines(&self) -> usize {
        let lines = self.rope.len_lines();
        let chars = self.rope.len_chars();
        if lines > 1 && self.rope.char(chars - 1) == '\n' {
            lines - 1
        } else {
            lines
        }
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

    /// Reconcile an explicit save policy without replacing text edited during the save.
    pub(crate) fn set_line_ending(&mut self, ending: crate::editor::text::LineEnding) {
        self.format.preferred_line_ending = ending;
        self.format.line_endings.fill(ending);
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
        ByteRange::new(range.start, range.end)?;
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

    pub(crate) fn line_endings(&self, range: ByteRange) -> Vec<crate::editor::text::LineEnding> {
        let first = self.byte_to_line(range.start).unwrap_or(0);
        let last = self.byte_to_line(range.end).unwrap_or(first);
        (first..last)
            .map(|index| {
                self.format
                    .line_endings
                    .get(index)
                    .copied()
                    .unwrap_or(self.format.preferred_line_ending)
            })
            .collect()
    }

    pub(crate) fn set_line_endings(
        &mut self,
        first: usize,
        endings: &[crate::editor::text::LineEnding],
    ) {
        for (index, ending) in endings.iter().enumerate() {
            if let Some(target) = self.format.line_endings.get_mut(first + index) {
                *target = *ending;
            }
        }
    }

    /// The text in rope-sized pieces, for hashing or comparing without building a string.
    pub fn chunks(&self) -> impl Iterator<Item = &str> {
        self.rope.chunks()
    }

    #[must_use]
    pub fn text_equals(&self, other: &str) -> bool {
        let mut rest = other.as_bytes();
        self.rope.len_bytes() == other.len()
            && self.rope.chunks().all(|chunk| {
                let (head, tail) = rest.split_at(chunk.len());
                rest = tail;
                head == chunk.as_bytes()
            })
    }

    /// Restore a historical snapshot while preserving monotonic revisions.
    pub fn restore_text(&mut self, text: &str) -> Result<(), PositionError> {
        self.bump_revision()?;
        self.rope = Rope::from_str(text);
        self.recent_edits.clear();
        self.recent_texts.clear();
        Ok(())
    }

    pub(in crate::editor::text) fn remember_edit(
        &mut self,
        before: BufferRevision,
        delta: EditDelta,
        previous: Rope,
    ) {
        if self.recent_edits.len() == RECENT_EDITS {
            self.recent_edits.remove(0);
        }
        self.recent_edits.push((before, delta));
        if previous.len_bytes() > RECENT_TEXT_BYTES {
            self.recent_texts.clear();
            return;
        }
        if self.recent_texts.len() == RECENT_TEXTS {
            self.recent_texts.remove(0);
        }
        self.recent_texts.push((before, previous));
    }

    /// The text as it was at `revision`, while that is among the last few edits.
    #[must_use]
    pub fn text_at(&self, revision: BufferRevision) -> Option<Self> {
        if revision == self.revision {
            return Some(self.clone());
        }
        let (_, rope) = self
            .recent_texts
            .iter()
            .find(|(before, _)| *before == revision)?;
        Some(Self {
            rope: rope.clone(),
            revision,
            format: self.format.clone(),
            source: self.source.clone(),
            recent_edits: Vec::new(),
            recent_texts: Vec::new(),
        })
    }

    /// Moves a range computed at `revision` onto the current text, or `None` when the edits
    /// since then are no longer remembered or the range was deleted.
    #[must_use]
    pub fn map_range_since(&self, revision: BufferRevision, range: ByteRange) -> Option<ByteRange> {
        if revision == self.revision {
            return Some(range);
        }
        let start = self.map_offset_since(revision, range.start)?;
        let end = self.map_offset_since(revision, range.end)?;
        ByteRange::new(start, end)
            .ok()
            .filter(|range| !range.is_empty())
    }

    /// Moves an offset taken at `revision` onto the current text, or `None` when the edits
    /// since then are no longer remembered.
    #[must_use]
    pub fn map_offset_since(
        &self,
        revision: BufferRevision,
        offset: ByteOffset,
    ) -> Option<ByteOffset> {
        if revision == self.revision {
            return Some(offset);
        }
        let first = self
            .recent_edits
            .iter()
            .position(|(before, _)| *before == revision)?;
        Some(
            self.recent_edits[first..]
                .iter()
                .fold(offset, |offset, (_, delta)| {
                    delta.map_offset(offset, Affinity::After)
                }),
        )
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
    use crate::editor::text::rope::*;
    #[test]
    fn reversed_ranges_are_rejected_without_changing_text() {
        use crate::editor::text::EditError;
        use crate::editor::text::TextEdit;
        use crate::editor::text::TextEditSet;
        let mut text = TextBuffer::from_text("abc");
        let revision = text.revision();
        let range = ByteRange {
            start: ByteOffset(2),
            end: ByteOffset(1),
        };
        let error = PositionError::ReversedRange { start: 2, end: 1 };
        assert_eq!(text.validate_range(range), Err(error.clone()));
        assert_eq!(text.slice(range), Err(error.clone()));
        assert_eq!(
            text.apply_edits(&TextEditSet::new(revision, vec![TextEdit::new(range, "x")])),
            Err(EditError::InvalidPosition(error))
        );
        assert_eq!(text.to_string(), "abc");
        assert_eq!(text.revision(), revision);
    }

    #[test]
    fn protocol_lines_are_delimited_only_by_lf() {
        let text = TextBuffer::from_text("a\u{000c}b\u{2028}d\nc");
        assert_eq!(text.len_lines(), 2);
        let at = crate::editor::text::PositionCodec::new(&text)
            .lsp_to_byte(1, 0, crate::editor::text::PositionEncoding::Utf8)
            .unwrap();
        assert_eq!(&text.to_string()[at.get()..], "c");
    }

    #[test]
    fn exact_snapshots_distinguish_branches_and_reload_but_keep_edit_instance() {
        let original = TextBuffer::from_text("before");
        let mut a = original.clone();
        let mut b = original.clone();
        a.restore_text("branch a").unwrap();
        b.restore_text("branch b").unwrap();
        assert_eq!(a.revision(), b.revision());
        assert_ne!(a.source_version(), b.source_version());
        assert!(a.source_version().same_instance(&b.source_version()));
        assert!(
            !a.source_version()
                .same_instance(&TextBuffer::from_text("branch a").source_version())
        );
        a.revision = BufferRevision(u64::MAX);
        let source = a.source_version();
        let bytes = a.to_string();
        assert!(a.restore_text("must not publish").is_err());
        assert_eq!(a.to_string(), bytes);
        assert_eq!(a.source_version(), source);
    }

    #[test]
    fn a_highlight_follows_the_edits_made_after_it() {
        use crate::editor::text::TextEdit;
        use crate::editor::text::TextEditSet;
        let mut text = TextBuffer::from_text("// note\nfn main() {}\n");
        let parsed = text.revision();
        let main = ByteRange::new(ByteOffset(11), ByteOffset(15)).unwrap();
        let comment = ByteRange::new(ByteOffset(0), ByteOffset(7)).unwrap();
        for (at, insert) in [(7, "s"), (0, "x")] {
            let edit = TextEdit::new(ByteRange::empty(ByteOffset(at)), insert);
            text.apply_edits(&TextEditSet::new(text.revision(), vec![edit]))
                .unwrap();
        }
        assert_eq!(&text.to_string()[13..17], "main");
        assert_eq!(
            text.map_range_since(parsed, main),
            Some(ByteRange::new(ByteOffset(13), ByteOffset(17)).unwrap())
        );
        // Typing at the end of the comment keeps the new letter in its colour.
        assert_eq!(
            text.map_range_since(parsed, comment),
            Some(ByteRange::new(ByteOffset(1), ByteOffset(9)).unwrap())
        );
        text.restore_text("other").unwrap();
        assert_eq!(text.map_range_since(parsed, main), None);
    }
}
