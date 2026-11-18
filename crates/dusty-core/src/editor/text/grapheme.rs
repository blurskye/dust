//! Grapheme boundaries and navigation.

use unicode_segmentation::GraphemeCursor;
use unicode_segmentation::GraphemeIncomplete;
use unicode_width::UnicodeWidthStr;

use crate::editor::text::ByteOffset;

use crate::editor::text::TextBuffer;

#[must_use]
pub fn next_grapheme_boundary(buffer: &TextBuffer, offset: ByteOffset) -> ByteOffset {
    grapheme_boundary(buffer, offset, true)
}

#[must_use]
pub fn previous_grapheme_boundary(buffer: &TextBuffer, offset: ByteOffset) -> ByteOffset {
    grapheme_boundary(buffer, offset, false)
}

fn grapheme_boundary(buffer: &TextBuffer, offset: ByteOffset, forward: bool) -> ByteOffset {
    let mut cursor = GraphemeCursor::new(offset.get(), buffer.len_bytes(), true);
    let (mut chunk, mut start, _, _) = buffer.rope.chunk_at_byte(offset.get());
    loop {
        let result = if forward {
            cursor.next_boundary(chunk, start)
        } else {
            cursor.prev_boundary(chunk, start)
        };
        match result {
            Ok(boundary) => {
                return ByteOffset(boundary.unwrap_or(if forward {
                    buffer.len_bytes()
                } else {
                    0
                }));
            }
            Err(GraphemeIncomplete::NextChunk) => {
                (chunk, start, _, _) = buffer.rope.chunk_at_byte(start + chunk.len());
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, start, _, _) = buffer.rope.chunk_at_byte(start - 1);
            }
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (context, context_start, _, _) = buffer.rope.chunk_at_byte(end - 1);
                cursor.provide_context(&context[..end - context_start], context_start);
            }
            Err(GraphemeIncomplete::InvalidOffset) => {
                unreachable!("cursor stays in the supplied rope chunk")
            }
        }
    }
}

#[must_use]
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod tests {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    use crate::editor::text::next_grapheme_boundary;
    use crate::editor::text::previous_grapheme_boundary;

    #[test]
    fn navigates_large_rope_without_materializing_text() {
        let buffer = TextBuffer::from_text(&"a".repeat(1 << 20));
        let before = TextBuffer::test_materializations();
        let mut offset = ByteOffset((1 << 19) - 500);
        for _ in 0..1000 {
            offset = next_grapheme_boundary(&buffer, offset);
        }
        assert_eq!(offset, ByteOffset((1 << 19) + 500));
        for _ in 0..1000 {
            offset = previous_grapheme_boundary(&buffer, offset);
        }
        assert_eq!(offset, ByteOffset((1 << 19) - 500));
        assert_eq!(TextBuffer::test_materializations(), before);
    }

    #[test]
    fn regional_indicators_use_preceding_context_inside_a_cluster() {
        let buffer = TextBuffer::from_text("🇦🇧🇨🇩🇪");
        assert_eq!(
            next_grapheme_boundary(&buffer, ByteOffset(4)),
            ByteOffset(8)
        );
        assert_eq!(
            previous_grapheme_boundary(&buffer, ByteOffset(12)),
            ByteOffset(8)
        );
    }

    #[test]
    fn rope_chunk_boundaries_match_whole_text_segmentation() {
        use unicode_segmentation::UnicodeSegmentation;

        for text in [
            format!("{}z", "a\u{301}".repeat(1000)),
            format!("{}z", "👩‍💻".repeat(500)),
            format!("{}z", "🇦".repeat(501)),
            format!("a{}z", "\u{301}".repeat(1500)),
            String::new(),
        ] {
            let buffer = TextBuffer::from_text(&text);
            let mut boundaries: Vec<_> = text.grapheme_indices(true).map(|(at, _)| at).collect();
            boundaries.push(text.len());
            for pair in boundaries.windows(2) {
                assert_eq!(
                    next_grapheme_boundary(&buffer, ByteOffset(pair[0])),
                    ByteOffset(pair[1])
                );
                assert_eq!(
                    previous_grapheme_boundary(&buffer, ByteOffset(pair[1])),
                    ByteOffset(pair[0])
                );
            }
            assert_eq!(
                next_grapheme_boundary(&buffer, ByteOffset(text.len())),
                ByteOffset(text.len())
            );
            assert_eq!(
                previous_grapheme_boundary(&buffer, ByteOffset(0)),
                ByteOffset(0)
            );
        }
    }

    #[test]
    fn navigates_combining_and_emoji_graphemes() {
        let buffer = TextBuffer::from_text("a\u{301}👩‍💻z");
        let first = next_grapheme_boundary(&buffer, ByteOffset(0));
        assert_eq!(first, ByteOffset("a\u{301}".len()));
        let second = next_grapheme_boundary(&buffer, first);
        assert_eq!(second, ByteOffset("a\u{301}👩‍💻".len()));
        assert_eq!(previous_grapheme_boundary(&buffer, second), first);
    }
}
