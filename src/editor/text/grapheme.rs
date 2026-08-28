use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::editor::text::{ByteOffset, TextBuffer};

#[must_use]
pub fn next_grapheme_boundary(buffer: &TextBuffer, offset: ByteOffset) -> ByteOffset {
    let text = buffer.to_string();
    text[offset.get()..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(ByteOffset(text.len()), |(relative, _)| {
            ByteOffset(offset.get() + relative)
        })
}

#[must_use]
pub fn previous_grapheme_boundary(buffer: &TextBuffer, offset: ByteOffset) -> ByteOffset {
    let text = buffer.to_string();
    text[..offset.get()]
        .grapheme_indices(true)
        .next_back()
        .map_or(ByteOffset(0), |(index, _)| ByteOffset(index))
}

#[must_use]
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod tests {
    use crate::editor::text::{
        ByteOffset, TextBuffer, next_grapheme_boundary, previous_grapheme_boundary,
    };

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
