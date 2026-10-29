//! Flash-style jump (`s`), as flash.nvim in LazyVim: type a few characters, every visible
//! match gets a one-key label, and typing that label jumps there.

use crate::editor::{
    ViewId,
    text::{ByteOffset, ByteRange, TextBuffer},
};

/// Home row first, like flash.nvim.
const LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";

#[derive(Clone, Debug)]
pub(super) struct FlashState {
    pub(super) view: ViewId,
    pub(super) query: String,
    pub(super) targets: Vec<FlashTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FlashTarget {
    pub(super) range: ByteRange,
    pub(super) label: Option<char>,
}

/// Every match of `query` in lines `first..=last`, labelled nearest to the cursor first.
///
/// Matching ignores case unless the query has a capital letter. A label is never a character
/// that continues some match, so typing more of the query is never read as a label.
pub(super) fn flash_targets(
    text: &TextBuffer,
    first: usize,
    last: usize,
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
    let last = last.min(text.len_lines().saturating_sub(1));
    for line in first..=last {
        let (Ok(start), Ok(content)) = (text.line_to_byte(line), text.line(line)) else {
            continue;
        };
        let content = content.trim_end_matches(['\n', '\r']);
        for (index, _) in content.char_indices() {
            let mut rest = content[index..].chars();
            if !query.chars().all(|wanted| rest.next().is_some_and(|got| same(wanted, got))) {
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
    let mut labels = LABELS.chars().filter(|label| !continuations.contains(label));
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
    use super::*;

    fn labels(text: &str, cursor: usize, query: &str) -> Vec<(usize, Option<char>)> {
        let text = TextBuffer::from_bytes(text.as_bytes()).unwrap();
        flash_targets(&text, 0, 100, ByteOffset(cursor), query)
            .into_iter()
            .map(|target| (target.range.start.get(), target.label))
            .collect()
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
