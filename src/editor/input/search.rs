//! Search patterns and buffer match navigation.

use std::ops::Range;

use regex::RegexBuilder;
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SearchPatternError {
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
}

pub fn search_matches(
    source: &str,
    query: &str,
    smart_case: bool,
    regex: bool,
) -> Result<Vec<Range<usize>>, SearchPatternError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let insensitive = smart_case && !query.chars().any(char::is_uppercase);
    if regex {
        let expression = RegexBuilder::new(query)
            .case_insensitive(insensitive)
            .build()
            .map_err(|error| SearchPatternError::InvalidRegex(error.to_string()))?;
        return Ok(expression
            .find_iter(source)
            .map(|matched| matched.start()..matched.end())
            .collect());
    }
    let expression = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(insensitive)
        .build()
        .map_err(|error| SearchPatternError::InvalidRegex(error.to_string()))?;
    let mut matches = Vec::new();
    let mut from = 0;
    while let Some(found) = expression.find_at(source, from) {
        matches.push(found.start()..found.end());
        from = found.start()
            + source[found.start()..]
                .chars()
                .next()
                .expect("literal is nonempty")
                .len_utf8();
    }
    Ok(matches)
}

pub fn substitution_regex(pattern: &str, flags: &str) -> Result<regex::Regex, String> {
    if flags.chars().any(|flag| !"gicn".contains(flag)) {
        return Err("unsupported substitute flag".into());
    }
    let pattern = crate::editor::input::substitution_pattern(pattern);
    RegexBuilder::new(&pattern)
        .case_insensitive(flags.contains('i'))
        .build()
        .map_err(|error| error.to_string())
}

/// Find one occurrence without retaining a range for every match in a large file.
pub fn search_match(
    source: &str,
    query: &str,
    smart_case: bool,
    whole_word: bool,
    offset: usize,
    forward: bool,
    wrap: bool,
) -> Result<Option<Range<usize>>, SearchPatternError> {
    if query.is_empty() {
        return Ok(None);
    }
    let pattern = if whole_word {
        format!(r"\b(?:{})\b", regex::escape(query))
    } else {
        regex::escape(query)
    };
    let expression = RegexBuilder::new(&pattern)
        .case_insensitive(smart_case && !query.chars().any(char::is_uppercase))
        .build()
        .map_err(|error| SearchPatternError::InvalidRegex(error.to_string()))?;
    let matches = || {
        let mut from = 0;
        let expression = &expression;
        std::iter::from_fn(move || {
            let found = expression.find_at(source, from)?;
            from = found.start()
                + source[found.start()..]
                    .chars()
                    .next()
                    .expect("literal is nonempty")
                    .len_utf8();
            Some(found)
        })
    };
    let found = if forward {
        let found = expression.find_at(source, offset);
        found.or_else(|| if wrap { matches().next() } else { None })
    } else {
        matches()
            .take_while(|found| found.start() < offset)
            .last()
            .or_else(|| if wrap { matches().last() } else { None })
    };
    Ok(found.map(|found| found.start()..found.end()))
}

#[cfg(test)]
mod tests {
    use crate::editor::input::search::search_matches;

    #[test]
    fn navigation_finds_one_match_in_repetitive_large_text() {
        let source = "a".repeat(16 * 1024 * 1024);
        assert_eq!(
            crate::editor::input::search::search_match(&source, "a", true, false, 1, true, false)
                .unwrap(),
            Some(1..2)
        );
        assert_eq!(
            crate::editor::input::search::search_match(
                "foo foobar foo",
                "foo",
                true,
                true,
                1,
                true,
                false
            )
            .unwrap(),
            Some(11..14)
        );
        assert_eq!(
            crate::editor::input::search::search_match(
                "foo foobar foo",
                "foo",
                true,
                true,
                11,
                false,
                true
            )
            .unwrap(),
            Some(0..3)
        );
    }
    #[test]
    fn whole_word_search_includes_combining_marks() {
        assert_eq!(
            crate::editor::input::search::search_match(
                "e\u{301} e",
                "e",
                true,
                true,
                0,
                true,
                false
            )
            .unwrap(),
            Some(4..5)
        );
        assert_eq!(
            crate::editor::input::search::search_match(
                "e\u{301} e e\u{301}",
                "e\u{301}",
                true,
                true,
                1,
                true,
                false
            )
            .unwrap(),
            Some(6..9)
        );
    }

    #[test]
    fn navigation_uses_unicode_smart_case() {
        assert_eq!(
            crate::editor::input::search::search_match("É é", "é", true, false, 0, true, false)
                .unwrap(),
            Some(0..2)
        );
        assert_eq!(
            crate::editor::input::search::search_match("É é", "é", false, false, 0, true, false)
                .unwrap(),
            Some(3..5)
        );
        assert_eq!(
            crate::editor::input::search::search_match("é É", "É", true, false, 0, true, false)
                .unwrap(),
            Some(3..5)
        );
    }

    #[test]
    fn backward_navigation_keeps_overlapping_literal_starts() {
        for offset in [5, 0] {
            assert_eq!(
                crate::editor::input::search::search_match(
                    "banana", "ana", true, false, offset, false, true
                )
                .unwrap(),
                Some(3..6)
            );
        }
        assert_eq!(
            crate::editor::input::search::search_match(
                "banana", "ana", true, false, 3, false, false
            )
            .unwrap(),
            Some(1..4)
        );
    }

    #[test]
    fn literal_smart_case_and_regex_are_explicit_policies() {
        assert_eq!(
            search_matches("One one", "one", true, false).expect("literal"),
            vec![0..3, 4..7]
        );
        assert_eq!(
            search_matches("One one", "One", true, false).expect("case"),
            vec![0..3]
        );
        assert_eq!(
            search_matches("abc 123", r"\d+", true, true).expect("regex"),
            vec![4..7]
        );
        assert!(search_matches("x", "[", true, true).is_err());
    }
    #[test]
    fn literal_search_folds_unicode_and_keeps_overlapping_starts() {
        assert_eq!(
            search_matches("É é", "é", true, false).unwrap(),
            vec![0..2, 3..5]
        );
        assert_eq!(
            search_matches("banana", "ana", true, false).unwrap(),
            vec![1..4, 3..6]
        );
        assert_eq!(
            search_matches("banana", "ana", false, false).unwrap(),
            vec![1..4, 3..6]
        );
    }
}
