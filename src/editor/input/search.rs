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
    if insensitive && query.is_ascii() {
        let query = query.to_ascii_lowercase();
        return Ok(source
            .to_ascii_lowercase()
            .match_indices(&query)
            .map(|(start, value)| start..start + value.len())
            .collect());
    }
    Ok(source
        .match_indices(query)
        .map(|(start, value)| start..start + value.len())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::search_matches;

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
}
