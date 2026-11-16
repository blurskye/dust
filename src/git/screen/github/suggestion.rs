//! GitHub's suggested changes: a ```` ```suggestion ```` block in a review
//! comment reads as the lines it replaces (`−`) and the lines it suggests (`+`), not as code.

use super::lines::Line;
use super::prose::{Rich, role, strong};
use super::*;
use crate::forge::request::Write;
use crate::ui::{GitRole, GitSurface};
use unicode_width::UnicodeWidthStr;

/// A comment body cut at its suggestion blocks.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Part {
    Text(String),
    /// The suggested lines (empty: delete the commented lines).
    Suggestion(Vec<String>),
}

/// The fence that opens a suggestion: three or more backticks or tildes, then `suggestion`.
fn opens(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim();
    let mark = trimmed
        .chars()
        .next()
        .filter(|mark| *mark == '`' || *mark == '~')?;
    let run = trimmed.chars().take_while(|ch| *ch == mark).count();
    (run >= 3 && trimmed[run..].trim() == "suggestion").then_some((mark, run))
}

fn closes(line: &str, (mark, run): (char, usize)) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= run && trimmed.chars().all(|ch| ch == mark)
}

/// `body` cut into prose and suggestion blocks; `None` when it holds no suggestion.
pub(super) fn split(body: &str) -> Option<Vec<Part>> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut open: Option<((char, usize), Vec<String>)> = None;
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match &mut open {
            Some((fence, lines)) => {
                if closes(line, *fence) {
                    parts.push(Part::Suggestion(std::mem::take(lines)));
                    open = None;
                } else {
                    lines.push(line.to_owned());
                }
            }
            None => match opens(line) {
                Some(fence) => {
                    if !text.trim().is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    text.clear();
                    open = Some((fence, Vec::new()));
                }
                None => {
                    text.push_str(line);
                    text.push('\n');
                }
            },
        }
    }
    // An unclosed block runs to the end, as in GitHub's Markdown.
    if let Some((_, lines)) = open {
        parts.push(Part::Suggestion(lines));
    }
    if !text.trim().is_empty() {
        parts.push(Part::Text(text));
    }
    parts
        .iter()
        .any(|part| matches!(part, Part::Suggestion(_)))
        .then_some(parts)
}

/// The suggested lines of the first suggestion in `body`.
pub(super) fn first(body: &str) -> Option<Vec<String>> {
    split(body)?.into_iter().find_map(|part| match part {
        Part::Suggestion(lines) => Some(lines),
        Part::Text(_) => None,
    })
}

/// The `count` lines a comment on the end of `hunk` covers: the last lines of its new side.
pub(super) fn original(hunk: &str, count: usize) -> Vec<String> {
    let new_side = hunk
        .lines()
        .filter(|line| !line.starts_with("@@") && !line.starts_with('-') && !line.starts_with('\\'))
        .map(|line| line.get(1..).unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    new_side[new_side.len().saturating_sub(count)..].to_vec()
}

/// How many lines a comment on `start..=line` covers.
pub(super) fn span(start: Option<u64>, line: Option<u64>) -> usize {
    match (start, line) {
        (Some(start), Some(line)) if start <= line => (line - start + 1) as usize,
        _ => 1,
    }
}

/// `Suggested change`, then the replaced lines and the suggested ones, as a diff.
pub(super) fn rows(
    rows: &mut Vec<Line>,
    original: &[String],
    suggested: &[String],
    width: usize,
    indent: &str,
) {
    let mut heading = Rich::new()
        .plain(indent)
        .push("Suggested change", strong(GitRole::Text));
    if original.is_empty() {
        heading = heading.sep().push(
            "the lines it replaces are not in this diff",
            role(GitRole::Muted),
        );
    }
    let mut heading = heading.line();
    heading.band = Some(GitSurface::Code);
    rows.push(heading);
    let room = width.saturating_sub(indent.width() + 2).max(8);
    let sides = original
        .iter()
        .map(|line| ('−', line))
        .chain(suggested.iter().map(|line| ('+', line)));
    for (sign, line) in sides {
        let added = sign == '+';
        let text = super::super::shell::chrome::ellipsize(&line.replace('\t', "    "), room);
        let tone = if added {
            GitRole::Added
        } else {
            GitRole::Removed
        };
        let mut row = Rich::new()
            .plain(indent)
            .push(format!("{sign} "), role(tone))
            .push(text, role(tone))
            .line();
        row.band = Some(if added {
            GitSurface::AddedBand
        } else {
            GitSurface::RemovedBand
        });
        rows.push(row);
    }
    if suggested.is_empty() {
        let mut row = Rich::new()
            .plain(indent)
            .push("Deletes these lines", role(GitRole::Muted))
            .line();
        row.band = Some(GitSurface::Code);
        rows.push(row);
    }
}

/// A thread's suggestion, ready to commit: lines `start..=end` of `path`.
pub(super) struct Found {
    pub(super) path: String,
    pub(super) start: u64,
    pub(super) end: u64,
    /// The lines it replaces, from the thread's hunk; empty when the hunk does not reach them.
    pub(super) original: Vec<String>,
    pub(super) replacement: Vec<String>,
}

/// The first suggestion in a review thread (the forge's thread JSON); `None` when it has none
/// or the thread is outdated (its line is gone from the diff).
pub(super) fn found(thread: &Value) -> Option<Found> {
    let comments = thread["comments"]["nodes"].as_array()?;
    let (comment, replacement) = comments
        .iter()
        .find_map(|comment| Some((comment, first(comment["body"].as_str()?)?)))?;
    let end = thread["line"]
        .as_u64()
        .or_else(|| comment["line"].as_u64())?;
    let start = thread["startLine"]
        .as_u64()
        .filter(|start| *start <= end)
        .unwrap_or(end);
    let path = thread["path"]
        .as_str()
        .filter(|path| !path.is_empty())
        .or_else(|| comment["path"].as_str())?
        .to_owned();
    let count = span(Some(start), Some(end));
    let original = original(comment["diffHunk"].as_str().unwrap_or_default(), count);
    Some(Found {
        path,
        start,
        end,
        original: if original.len() == count {
            original
        } else {
            Vec::new()
        },
        replacement,
    })
}

fn lines_text(start: u64, end: u64) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}–{end}")
    }
}

/// The default commit message, GitHub's.
const MESSAGE: &str = "Apply suggestion from code review";

impl Workspace {
    fn suggestion(&self, number: u64, thread: &str) -> Result<Found, String> {
        self.review_threads(number)
            .into_iter()
            .find(|candidate| candidate["id"].as_str() == Some(thread))
            .and_then(found)
            .ok_or_else(|| "That suggestion is no longer on the pull request · R refreshes".into())
    }

    /// Whether a thread offers `Commit suggestion`: an open thread with a suggestion on a line
    /// still in the diff, on GitHub, signed in.
    pub(super) fn offers_commit(&self, number: u64, thread: &str) -> bool {
        use crate::forge::provider::ProviderKind;
        !matches!(
            self.forge_kind(),
            Some(ProviderKind::GitLab | ProviderKind::Gitea | ProviderKind::Forgejo)
        ) && !self.read_only()
            && self
                .review_threads(number)
                .into_iter()
                .find(|candidate| candidate["id"].as_str() == Some(thread))
                .is_some_and(|thread| {
                    thread["isResolved"].as_bool() == Some(false) && found(thread).is_some()
                })
    }

    /// The sheet for `Commit suggestion`: the commit message, GitHub's default filled in.
    pub(super) fn suggestion_form(&mut self, action: &Action) -> Result<Option<Form>, String> {
        let Action::CommitSuggestion(number, thread) = action else {
            return Ok(None);
        };
        let found = self.suggestion(*number, thread)?;
        let fields = vec![super::actions::field(
            "Commit message",
            MESSAGE,
            true,
            false,
        )];
        let title = format!(
            "Commit suggestion to {}:{}",
            found.path,
            lines_text(found.start, found.end)
        );
        Ok(Some(self.form(&title, action.clone(), fields)))
    }

    /// The line under the sheet's title: where the commit goes and what it changes.
    pub(super) fn suggestion_note(&self, action: &Action) -> Option<String> {
        let Action::CommitSuggestion(number, thread) = action else {
            return None;
        };
        let found = self.suggestion(*number, thread).ok()?;
        let branch = self
            .item()
            .map(|item| string(item, "headRefName").to_owned())
            .unwrap_or_default();
        let replaced = (found.end - found.start + 1) as usize;
        let words = |count: usize| format!("{count} line{}", if count == 1 { "" } else { "s" });
        let change = if found.replacement.is_empty() {
            format!("deletes {}", words(replaced))
        } else {
            format!(
                "replaces {} with {}",
                words(replaced),
                words(found.replacement.len())
            )
        };
        Some(if branch.is_empty() {
            format!("Commits to the head branch · {change}")
        } else {
            format!("Commits to {branch} · {change}")
        })
    }

    /// The write for a confirmed `Commit suggestion`, pinned to the head it was read at.
    pub(super) fn suggestion_write(
        &self,
        number: u64,
        thread: &str,
        values: &[String],
    ) -> Result<Write, String> {
        let found = self.suggestion(number, thread)?;
        let head = self
            .value(&Key::Item(2, number))
            .map(|item| string(item, "headRefOid").to_owned())
            .unwrap_or_default();
        if head.is_empty() {
            return Err("Refresh the pull request before committing a suggestion".into());
        }
        let message = values
            .first()
            .map(|message| message.trim())
            .filter(|message| !message.is_empty())
            .unwrap_or(MESSAGE);
        Ok(Write::CommitSuggestion {
            number,
            path: found.path,
            start: found.start,
            end: found.end,
            original: found.original,
            replacement: found.replacement,
            head,
            message: message.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggestion_block_splits_from_the_prose_around_it() {
        let body = "Name it after what it bounds.\r\n```suggestion\r\n    bounded = min(attempt, 6)\r\n```\r\nThanks!";
        assert_eq!(
            split(body),
            Some(vec![
                Part::Text("Name it after what it bounds.\n".into()),
                Part::Suggestion(vec!["    bounded = min(attempt, 6)".into()]),
                Part::Text("Thanks!\n".into()),
            ])
        );
        assert_eq!(split("```rust\nfn main() {}\n```"), None);
        assert_eq!(first("````suggestion\n````"), Some(Vec::new()));
    }

    #[test]
    fn the_replaced_lines_are_the_end_of_the_hunks_new_side() {
        let hunk = "@@ -1,3 +1,4 @@\n keep\n-old\n+one\n+two\n\\ No newline at end of file";
        assert_eq!(original(hunk, 2), vec!["one".to_owned(), "two".to_owned()]);
        assert_eq!(
            original(hunk, 9),
            vec!["keep".to_owned(), "one".to_owned(), "two".to_owned()]
        );
        assert_eq!(span(Some(2), Some(4)), 3);
        assert_eq!(span(None, Some(4)), 1);
    }

    #[test]
    fn a_suggestion_reads_as_a_diff() {
        let mut out = Vec::new();
        rows(&mut out, &["old()".into()], &["new()".into()], 60, "  ");
        let text = out.iter().map(|row| row.text.as_str()).collect::<Vec<_>>();
        assert_eq!(text, vec!["  Suggested change", "  − old()", "  + new()"]);
        assert_eq!(out[1].band, Some(GitSurface::RemovedBand));
        assert_eq!(out[2].band, Some(GitSurface::AddedBand));
    }
}
