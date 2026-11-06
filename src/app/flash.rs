//! Flash-style jump (`s`), as flash.nvim in LazyVim: type a few characters, every visible
//! match gets a one-key label, and typing that label jumps there.

use unicode_segmentation::UnicodeSegmentation;

use crate::editor::ViewId;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::text::TextBuffer;

/// Home row first, like flash.nvim.
const LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";

#[derive(Clone, Debug)]
pub(super) struct FlashState {
    pub(super) view: ViewId,
    pub(super) buffer: crate::editor::BufferId,
    pub(super) source: crate::editor::text::TextVersion,
    pub(super) query: String,
    pub(super) targets: Vec<FlashTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FlashTarget {
    pub(super) range: ByteRange,
    pub(super) label: Option<char>,
}

/// Every match of `query` in visible source ranges, labelled nearest to the cursor first.
///
/// Matching ignores case unless the query has a capital letter. A label is never a character
/// that continues some match, so typing more of the query is never read as a label.
pub(super) fn flash_targets(
    text: &TextBuffer,
    ranges: &[ByteRange],
    cursor: ByteOffset,
    query: &str,
) -> Vec<FlashTarget> {
    if query.is_empty() {
        return Vec::new();
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let same = |a: char, b: char| {
        if ignore_case {
            a.to_lowercase().eq(b.to_lowercase())
        } else {
            a == b
        }
    };
    let mut targets = Vec::new();
    let mut continuations = Vec::new();
    for range in ranges {
        let Ok(content) = text.slice(*range) else {
            continue;
        };
        let start = range.start;
        for (index, _) in content.grapheme_indices(true) {
            let mut rest = content[index..].chars();
            if !query
                .chars()
                .all(|wanted| rest.next().is_some_and(|got| same(wanted, got)))
            {
                continue;
            }
            let begin = ByteOffset(start.get() + index);
            let end = ByteOffset(begin.get() + content[index..].len() - rest.as_str().len());
            if begin == cursor {
                continue;
            }
            if let Some(next) = rest.next() {
                continuations.extend(next.to_lowercase());
            }
            if let Ok(range) = ByteRange::new(begin, end) {
                targets.push(FlashTarget { range, label: None });
            }
        }
    }
    let mut order = (0..targets.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| targets[index].range.start.get().abs_diff(cursor.get()));
    let mut labels = LABELS
        .chars()
        .filter(|label| !continuations.contains(label));
    for index in order {
        let Some(label) = labels.next() else {
            break;
        };
        targets[index].label = Some(label);
    }
    targets
}

#[cfg(test)]
mod tests {
    use crate::app::flash::*;

    fn labels(text: &str, cursor: usize, query: &str) -> Vec<(usize, Option<char>)> {
        let text = TextBuffer::from_bytes(text.as_bytes()).unwrap();
        let visible = ByteRange::new(ByteOffset(0), ByteOffset(text.len_bytes())).unwrap();
        flash_targets(&text, &[visible], ByteOffset(cursor), query)
            .into_iter()
            .map(|target| (target.range.start.get(), target.label))
            .collect()
    }

    #[test]
    fn hidden_matches_do_not_consume_labels_or_exclude_continuations() {
        let text = TextBuffer::from_text("xa xs xa");
        let targets = flash_targets(
            &text,
            &[ByteRange::new(ByteOffset(3), ByteOffset(5)).unwrap()],
            ByteOffset(99),
            "x",
        );
        assert_eq!(
            targets,
            vec![FlashTarget {
                range: ByteRange::new(ByteOffset(3), ByteOffset(4)).unwrap(),
                label: Some('a'),
            }]
        );
    }

    #[test]
    fn matches_never_start_inside_joined_emoji() {
        assert!(labels("👩‍💻 👨‍💻", 0, "💻").is_empty());
        assert_eq!(labels("👩‍💻 x", 99, "👩‍💻")[0].0, 0);
    }

    #[test]
    fn matches_are_labelled_nearest_first_and_skip_continuations() {
        // "ab" is followed by 'c' and 'd', so neither can be a label.
        assert_eq!(
            labels("abc abd\nxab\n", 8, "ab"),
            vec![(0, Some('f')), (4, Some('s')), (9, Some('a'))]
        );
    }

    #[test]
    fn a_capital_makes_the_match_case_sensitive() {
        assert_eq!(labels("Foo foo", 3, "f").len(), 2);
        assert_eq!(labels("Foo foo", 3, "F"), vec![(0, Some('a'))]);
    }
}
