//! The issues a pull request description says it closes, for forges that do not report them
//! (Gitea, Forgejo): GitHub's closing keywords, `close`/`closes`/`closed`, `fix`/`fixes`/
//! `fixed` and `resolve`/`resolves`/`resolved` in any case, each followed (an optional `:`
//! between) by `#N` or `owner/repo#N`. Code spans and fenced blocks never close anything.
use regex::Regex;
use std::sync::LazyLock;

static KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^\w/])(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?):?[ \t]+([A-Za-z0-9][\w.-]*/[\w.-]+)?#([0-9]+)\b",
    )
    .expect("closing keyword pattern")
});

/// One closing reference: the repository when the text named one other than `repo`, and the
/// issue number. In order of first mention, each once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Closing {
    pub repo: Option<String>,
    pub number: u64,
}

/// The issues `body` closes; `repo` (`owner/name`) is the pull request's own repository, so
/// `owner/name#N` naming it reads as `#N`.
pub fn closing_references(body: &str, repo: &str) -> Vec<Closing> {
    let mut found: Vec<Closing> = Vec::new();
    for line in prose_lines(body) {
        for capture in KEYWORD.captures_iter(&line) {
            let Some(number) = capture.get(2).and_then(|number| number.as_str().parse::<u64>().ok()) else {
                continue;
            };
            if number == 0 {
                continue;
            }
            let repo = capture
                .get(1)
                .map(|named| named.as_str().to_owned())
                .filter(|named| !named.eq_ignore_ascii_case(repo));
            let closing = Closing { repo, number };
            if !found.contains(&closing) {
                found.push(closing);
            }
        }
    }
    found
}

/// The body's lines outside fenced code, with inline code spans blanked out.
fn prose_lines(body: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        let run = marker.map_or(0, |c| trimmed.chars().take_while(|each| *each == c).count());
        match (fence, marker) {
            (Some((open, length)), Some(c)) if c == open && run >= length && trimmed[run..].trim().is_empty() => {
                fence = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, Some(c)) if run >= 3 => {
                fence = Some((c, run));
                continue;
            }
            _ => {}
        }
        lines.push(without_code_spans(line));
    }
    lines
}

fn without_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let ticks = rest[start..].chars().take_while(|c| *c == '`').count();
        let after = &rest[start + ticks..];
        let closing = "`".repeat(ticks);
        match after.find(&closing) {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &after[end + ticks..];
            }
            None => {
                out.push_str(&rest[..start + ticks]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(body: &str) -> Vec<(Option<String>, u64)> {
        closing_references(body, "orbit/orbit")
            .into_iter()
            .map(|closing| (closing.repo, closing.number))
            .collect()
    }

    #[test]
    fn every_keyword_form_in_any_case_closes() {
        let body = "Closes #1\nfixes #2, RESOLVED #3\nclose #4; Fix: #5 and resolve #6\n\
                    closed #7 · Fixed #8 · Resolves #9";
        let found: Vec<u64> = numbers(body).into_iter().map(|(_, number)| number).collect();
        assert_eq!(found, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn other_repositories_keep_their_name_and_this_one_reads_as_plain() {
        let body = "Fixes octo/widgets#12 and closes orbit/orbit#13. Closes #13 again.";
        assert_eq!(numbers(body), vec![(Some("octo/widgets".to_owned()), 12), (None, 13)]);
    }

    #[test]
    fn mentions_without_a_keyword_or_inside_code_close_nothing() {
        let body = "See #4, related to #5.\nprefixes #6 and unfixes #7\n`closes #8` is an example\n\
                    ```\nCloses #9\n```\nhotfix/#10\nCloses #11abc";
        assert!(numbers(body).is_empty(), "{:?}", numbers(body));
    }
}
