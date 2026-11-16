//! The reader's rows (`Line`), the reading position that survives reflow, and the row
//! builders the Pull requests and Issues readers share. Actions paints in `runs.rs`.
use super::*;
use crate::config::Highlight;
use crate::ui::Style;
use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SourceScope {
    Thread(String, usize, usize),
}
#[derive(Clone)]
pub(super) struct Line {
    pub(super) text: String,
    pub(super) source: Option<find::Source>,
    pub(super) style: Style,
    pub action: Option<Action>,
    pub(super) reply: Option<Action>,
    pub(super) spans: Vec<(Range<usize>, Highlight)>,
    pub(super) gutter: String,
    pub(super) gutter_style: Style,
    pub(super) outline: bool,
    pub(super) diff: Option<bool>,
    pub(super) header: bool,
    pub(super) pan: bool,
    pub(super) source_scope: Option<SourceScope>,
    pub(super) thread_header: bool,
    pub(super) thread_action: bool,
    pub(super) quiet_review: bool,
    /// Explicit styles that win over `spans` (forge reader rows: label chips, roles).
    pub(super) paint: Vec<(Range<usize>, Style)>,
    /// A background drawn across the whole row (code blocks, comment cards).
    pub(super) band: Option<crate::ui::GitSurface>,
    /// Backgrounds over byte ranges (the two sides of a split diff row).
    pub(super) bands: Vec<(Range<usize>, crate::ui::GitSurface)>,
    /// The Conversation's metadata sidebar (Reviewers, Labels…): painted in a right column
    /// beside the timeline when the reader is wide enough, else in place.
    pub(super) side: bool,
    /// The first row of a Conversation card: `j`/`k` step from card to card.
    pub(super) card: bool,
    /// The first row of an unresolved review thread: `n`/`N` step through them.
    pub(super) unresolved: bool,
    /// The card's own page on the forge, for `o`.
    pub(super) web: Option<String>,
    /// The merge box's header at the end of the Conversation, where `m` goes.
    pub(super) merge_box: bool,
}
impl Line {
    pub(super) fn text(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            source: None,
            style,
            action: None,
            reply: None,
            spans: Vec::new(),
            gutter: String::new(),
            gutter_style: Style::Muted,
            outline: false,
            diff: None,
            header: false,
            pan: true,
            source_scope: None,
            thread_header: false,
            thread_action: false,
            quiet_review: false,
            paint: Vec::new(),
            band: None,
            bands: Vec::new(),
            side: false,
            card: false,
            unresolved: false,
            web: None,
            merge_box: false,
        }
    }
    pub(super) fn button(text: impl Into<String>, action: Action) -> Self {
        let text = text.into();
        let text = text
            .strip_prefix("[ ")
            .and_then(|text| text.strip_suffix(" ]"))
            .unwrap_or(&text)
            .to_owned();
        let expandable = matches!(
            action,
            Action::Collapse(..)
                | Action::SelectFile(..)
                | Action::Files(_)
                | Action::Commit(_)
                | Action::PrTab(_)
        );
        let mut line = Self::text(
            if expandable {
                text
            } else {
                format!("› {text}")
            },
            Style::Default,
        );
        line.action = Some(action);
        line
    }
}
/// Logical source/byte anchors survive reflow in either direction without
/// recapturing the wider fragment's start and drifting on each focus round trip.
#[derive(Clone)]
struct ReadingAnchor {
    scope: Option<SourceScope>,
    logical: usize,
    text: std::sync::Arc<str>,
    byte: usize,
}
impl ReadingAnchor {
    fn capture(rows: &[Line], index: usize) -> Option<Self> {
        let source = rows.get(index)?.source.as_ref()?;
        let logical = rows[..=index]
            .iter()
            .filter(|row| row.source.as_ref().is_some_and(|source| source.offset == 0))
            .count()
            .checked_sub(1)?;
        Some(Self {
            scope: rows[index].source_scope.clone(),
            logical,
            text: source.text.clone(),
            byte: source.offset,
        })
    }
    fn locate(&self, rows: &[Line]) -> Option<usize> {
        let mut logical = 0usize;
        rows.iter().enumerate().find_map(|(index, row)| {
            let source = row.source.as_ref()?;
            if source.offset == 0 {
                logical += 1;
            }
            ((if let Some(scope) = &self.scope {
                row.source_scope.as_ref() == Some(scope)
            } else {
                row.source_scope.is_none() && logical == self.logical + 1
            }) && source.text == self.text
                && source.offset <= self.byte
                && (self.byte < source.offset + row.text.len() || row.text.is_empty()))
            .then_some(index)
        })
    }
}
/// Which occurrence of its action the row at `index` is: 0 for the first row carrying it.
/// Two rows can share an action (the sidebar's Development link and the timeline's mention
/// of the same issue), so an action alone does not say which row the cursor was on.
pub(super) fn action_ordinal(rows: &[Line], index: usize) -> usize {
    let Some(action) = rows.get(index).and_then(|row| row.action.as_ref()) else {
        return 0;
    };
    rows[..index]
        .iter()
        .filter(|row| row.action.as_ref() == Some(action))
        .count()
}

/// The row carrying `action` that stands for the cursor's: the same occurrence of it, else
/// the one nearest `near` (never simply the first, which would pull the cursor back up).
pub(super) fn action_row(
    rows: &[Line],
    action: &Action,
    ordinal: usize,
    near: usize,
) -> Option<usize> {
    let matches = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.action.as_ref() == Some(action))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    matches.get(ordinal).copied().or_else(|| {
        matches
            .iter()
            .copied()
            .min_by_key(|index| index.abs_diff(near))
    })
}

// Neutral thread rows have no source/action anchor. Scope their identity to
// the real Reply key, including the separator immediately after that thread.
fn neutral_thread_row(rows: &[Line], index: usize) -> Option<(&Action, bool, &str)> {
    let row = rows.get(index)?;
    if row.action.is_some() || row.source.is_some() {
        return None;
    }
    let reply = row.reply.as_ref().or_else(|| {
        if row.text.is_empty() {
            rows.get(index.checked_sub(1)?)
                .filter(|previous| previous.thread_action || previous.thread_header)
                .and_then(|previous| previous.reply.as_ref())
        } else {
            None
        }
    })?;
    Some((
        reply,
        row.thread_header,
        if row.thread_header {
            ""
        } else {
            row.text.as_str()
        },
    ))
}

#[derive(Clone, Default)]
pub(super) struct ReadingPosition {
    identity: Option<(String, String, Key, u64)>,
    cursor: Option<ReadingAnchor>,
    top: Option<ReadingAnchor>,
    rendered_cursor: usize,
    rendered_scroll: usize,
    pub(super) context: Option<String>,
    action: Option<Action>,
    reply: Option<Action>,
    projection: Vec<usize>,
    pub(super) narrow_pr: bool,
}
impl ReadingPosition {
    pub(super) fn restore(
        &mut self,
        identity: (String, String, Key, u64),
        old: &[Line],
        rows: &[Line],
        cursor: &mut usize,
        scroll: &mut usize,
    ) {
        if self.identity.as_ref() != Some(&identity) || old.is_empty() {
            *self = Self {
                identity: Some(identity),
                narrow_pr: self.narrow_pr,
                ..Self::default()
            };
            return;
        }
        let old_header = old.iter().take_while(|row| row.header).count();
        let header = rows.iter().take_while(|row| row.header).count();
        if self.cursor.is_none() || *cursor != self.rendered_cursor {
            self.cursor = ReadingAnchor::capture(old, *cursor);
        }
        if self.top.is_none() || *scroll != self.rendered_scroll {
            self.top = ReadingAnchor::capture(old, old_header + *scroll);
        }
        if let Some(index) = self.cursor.as_ref().and_then(|anchor| anchor.locate(rows)) {
            *cursor = index;
        } else if header > 0 && old.get(*cursor).is_some_and(|row| row.header) {
            *cursor = if *cursor + 1 == old_header {
                header - 1 // Metadata remains metadata when the title expands.
            } else {
                (*cursor).min(header.saturating_sub(2))
            };
        } else {
            self.cursor = ReadingAnchor::capture(rows, *cursor);
        }
        if let Some(index) = self.top.as_ref().and_then(|anchor| anchor.locate(rows)) {
            *scroll = index.saturating_sub(header);
        }
        self.rendered_cursor = *cursor;
        self.rendered_scroll = *scroll;
    }
    pub(super) fn finish(&mut self, rows: &[Line], cursor: usize, scroll: usize) {
        // Find/action reveal and viewport clamping can move after reflow.
        if cursor != self.rendered_cursor {
            self.cursor = ReadingAnchor::capture(rows, cursor);
        }
        if scroll != self.rendered_scroll {
            let header = rows.iter().take_while(|row| row.header).count();
            self.top = ReadingAnchor::capture(rows, header + scroll);
        }
        self.rendered_cursor = cursor;
        self.rendered_scroll = scroll;
    }
}
impl ReadingPosition {
    // Actions and source bytes are restored only inside the same semantic parent.
    // Keep the original byte anchor when a wider fragment contains it.
    pub(super) fn restore_scoped(
        &mut self,
        context: String,
        old: &[Line],
        rows: &[Line],
        cursor: &mut usize,
        scroll: usize,
    ) -> Option<usize> {
        if self.context.as_ref() != Some(&context) || old.is_empty() {
            // An ordinal from another head/attempt is not an action identity.
            // Explicit file reveal and an existing find match run afterwards.
            *cursor = 0;
            self.context = Some(context);
            self.cursor = None;
            self.top = None;
            self.action = None;
            self.reply = None;
            self.projection.clear();
            self.rendered_cursor = *cursor;
            self.rendered_scroll = scroll;
            return None;
        }
        let previous = old.get(*cursor);
        if *cursor != self.rendered_cursor || self.cursor.is_none() && self.action.is_none() {
            self.cursor = ReadingAnchor::capture(old, *cursor);
            self.action = previous.and_then(|row| row.action.clone());
            self.reply = previous.and_then(|row| row.reply.clone());
        }
        if scroll != self.rendered_scroll || self.top.is_none() {
            self.top = self
                .projection
                .get(scroll)
                .and_then(|index| ReadingAnchor::capture(old, *index));
        }
        let neutral = neutral_thread_row(old, *cursor);
        let restored = self
            .action
            .as_ref()
            .and_then(|action| {
                // A wrapped diff line's rows share its action: the source bytes say which
                // part the cursor is on, through a resize or a wrap toggle too.
                self.cursor
                    .as_ref()
                    .and_then(|anchor| anchor.locate(rows))
                    .filter(|&index| rows[index].action.as_ref() == Some(action))
                    .or_else(|| action_row(rows, action, action_ordinal(old, *cursor), *cursor))
            })
            .or_else(|| self.cursor.as_ref().and_then(|anchor| anchor.locate(rows)))
            .or_else(|| {
                let identity = neutral?;
                // Repeated author labels stay within this thread, by occurrence.
                let ordinal = (0..*cursor)
                    .filter(|&index| neutral_thread_row(old, index) == Some(identity))
                    .count();
                (0..rows.len())
                    .filter(|&index| neutral_thread_row(rows, index) == Some(identity))
                    .nth(ordinal)
            });
        if let Some(index) = restored {
            *cursor = index;
        } else if let Some(reply) = self.reply.as_ref().or_else(|| neutral.map(|row| row.0)) {
            // In particular, never change Resolve into Reopen under the cursor.
            *cursor = rows
                .iter()
                .position(|row| row.thread_header && row.reply.as_ref() == Some(reply))
                .unwrap_or(0);
            self.cursor = None;
            self.action = None;
        } else if let Some(previous) = previous {
            *cursor = (0..rows.len())
                .find(|&index| {
                    let row = &rows[index];
                    row.action.is_none()
                        && row.source.is_none()
                        && row.text == previous.text
                        && neutral_thread_row(rows, index).is_none()
                })
                .unwrap_or(0);
        }
        self.rendered_cursor = *cursor;
        self.rendered_scroll = scroll;
        self.top.as_ref().and_then(|anchor| anchor.locate(rows))
    }
    pub(super) fn finish_scoped(
        &mut self,
        rows: &[Line],
        cursor: usize,
        scroll: usize,
        main: Vec<usize>,
    ) {
        if cursor != self.rendered_cursor || self.cursor.is_none() {
            self.cursor = ReadingAnchor::capture(rows, cursor);
        }
        self.action = rows.get(cursor).and_then(|row| row.action.clone());
        self.reply = rows.get(cursor).and_then(|row| row.reply.clone());
        if scroll != self.rendered_scroll
            || self.top.is_none()
            || self.top.as_ref().and_then(|anchor| anchor.locate(rows)) != main.get(scroll).copied()
        {
            self.top = main
                .get(scroll)
                .and_then(|index| ReadingAnchor::capture(rows, *index));
        }
        self.rendered_cursor = cursor;
        self.rendered_scroll = scroll;
        self.projection = main;
    }
}

pub(super) fn position_context(workspace: &Workspace) -> String {
    let parent = if workspace.page < 3 {
        format!(
            "{:?}/{}/{}/{}/{:?}/{}",
            workspace.list_key(),
            workspace.number(),
            workspace.pr_tab,
            workspace
                .item()
                .map(|item| workspace.selected_file(workspace.number(), item))
                .unwrap_or(""),
            workspace.selected_commit,
            workspace
                .item()
                .map(|item| string(item, "headRefOid"))
                .unwrap_or("")
        )
    } else {
        String::new()
    };
    format!(
        "{}/{}/{}/{parent}",
        workspace.host, workspace.repo, workspace.page
    )
}

fn body(rows: &mut Vec<Line>, text: &str, width: usize) {
    let mut styled = crate::ui::markdown::Styled::default();
    crate::ui::markdown::render_logical(&mut styled, text);
    let mut logical_start = 0;
    for logical in styled.text.split_terminator('\n') {
        let source = std::sync::Arc::<str>::from(logical);
        let mut offset = 0;
        let preformatted = styled
            .preformatted
            .iter()
            .any(|range| range.contains(&logical_start));
        let fragments = if preformatted {
            vec![logical.to_owned()]
        } else {
            crate::ui::render::wrap_plain_text(logical, width.clamp(10, 80), styled.ambiguous_width)
        };
        for line in fragments {
            let start = logical_start + offset;
            let end = start + line.len();
            let first = styled
                .spans
                .partition_point(|(range, _)| range.end <= start);
            let mut row = Line::text(&line, Style::Default);
            row.pan = preformatted;
            row.spans = styled.spans[first..]
                .iter()
                .take_while(|(range, _)| range.start < end)
                .map(|(range, role)| {
                    (
                        range.start.max(start) - start..range.end.min(end) - start,
                        *role,
                    )
                })
                .collect();
            row.source = Some(find::Source {
                text: source.clone(),
                offset,
            });
            offset += line.len();
            rows.push(row);
        }
        logical_start += logical.len() + 1;
    }
}
/// A review comment's body; a ```` ```suggestion ```` block in it reads as the change it
/// suggests, against the lines the thread covers at the end of its diff hunk.
fn suggested_body(rows: &mut Vec<Line>, thread: &Value, comment: &Value, width: usize) {
    use super::suggestion::{self, Part};
    let text = string(comment, "body");
    let Some(parts) = suggestion::split(text) else {
        body(rows, text, width);
        return;
    };
    let line = thread["line"].as_u64().or_else(|| comment["line"].as_u64());
    let count = suggestion::span(thread["startLine"].as_u64(), line);
    let original = suggestion::original(string(comment, "diffHunk"), count);
    for part in parts {
        match part {
            Part::Text(text) => body(rows, &text, width),
            Part::Suggestion(lines) => suggestion::rows(rows, &original, &lines, width, ""),
        }
    }
}
pub(super) fn error_or_loading(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            // In words with one key: never the forge's status line or a stock gh hint.
            let words = workspace.explain(error);
            rows.push(Line::text(&words.headline, Style::Error));
            rows.push(Line::text(&words.hint, Style::Muted));
            true
        }
        None => {
            rows.push(Line::text("Loading…", Style::Muted));
            true
        }
    }
}
fn thread_reply_count(thread: &Value, loaded: usize) -> Option<String> {
    if loaded == 0 {
        return None;
    }
    let replies = loaded - 1;
    let total = thread["comments"]["totalCount"]
        .as_u64()
        .filter(|total| *total >= loaded as u64);
    if let Some(total) = total
        && total > loaded as u64
    {
        let total_replies = total - 1;
        return Some(format!(
            "{replies} of {total_replies} {} loaded",
            if total_replies == 1 {
                "reply"
            } else {
                "replies"
            }
        ));
    }
    (replies > 0).then(|| {
        format!(
            "{replies} {}{}",
            if replies == 1 { "reply" } else { "replies" },
            if total.is_some() { "" } else { " loaded" }
        )
    })
}
pub(super) fn threads(
    workspace: &Workspace,
    id: u64,
    rows: &mut Vec<Line>,
    width: usize,
    path: Option<&str>,
) {
    if error_or_loading(workspace, &Key::Threads(id), rows) {
        return;
    }
    for thread in workspace.review_threads(id) {
        let comments = array(&thread["comments"], "nodes");
        if path.is_some_and(|path| {
            !comments
                .iter()
                .any(|comment| string(comment, "path") == path)
        }) {
            continue;
        }
        let thread_id = string(thread, "id");
        let reply = Action::Reply {
            number: id,
            thread: thread_id.into(),
            repo: workspace.repo.clone(),
            host: workspace.host.clone(),
        };
        let start = rows.len();
        let resolved = thread["isResolved"].as_bool();
        let state = match resolved {
            Some(true) => "● Resolved",
            Some(false) => "◔ Unresolved",
            // Signed out, REST does not say whether a thread is resolved: the header names it
            // as GitHub does, without guessing.
            None => "Conversation",
        };
        let mut header = Line::text(state, Style::Muted);
        if let Some(first) = comments.first() {
            let current = first["line"].as_u64();
            let original = first["originalLine"].as_u64();
            let file = string(first, "path");
            if !file.is_empty() && path == Some(file) {
                header.text = format!("{state} · {}", login(first));
                if let Some(line) = current.or(original) {
                    header.text.push_str(&format!(
                        " · {}line {line}",
                        if current.is_some() { "" } else { "original " }
                    ));
                }
            } else {
                let anchor = current
                    .or(original)
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default();
                header.text = format!("{state} · {} · {file}{anchor}", login(first));
            }
        }
        if let Some(count) = thread_reply_count(thread, comments.len()) {
            header.text.push_str(&format!(" · {count}"));
        }
        header.thread_header = true;
        rows.push(header);
        for (comment_index, comment) in comments.iter().enumerate() {
            if comment_index > 0 {
                rows.push(Line::text(format!("  ↳ {}", login(comment)), Style::Muted));
                if comments.first().is_some_and(|first| {
                    string(first, "path") != string(comment, "path")
                        || first["line"] != comment["line"]
                        || first["originalLine"] != comment["originalLine"]
                }) {
                    let anchor = comment["line"]
                        .as_u64()
                        .or_else(|| comment["originalLine"].as_u64())
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default();
                    rows.push(Line::text(
                        format!("{}{anchor}", string(comment, "path")),
                        Style::Muted,
                    ));
                }
            }
            if comment["line"].is_null() && comment["originalLine"].is_number() {
                rows.push(Line::text("Outdated · original line", Style::Muted));
            }
            let body_start = rows.len();
            suggested_body(rows, thread, comment, width);
            let mut logical = 0;
            for row in &mut rows[body_start..] {
                if row.source.as_ref().is_some_and(|source| source.offset == 0) {
                    logical += 1;
                }
                row.source_scope = Some(SourceScope::Thread(
                    thread_id.into(),
                    comment_index,
                    logical,
                ));
            }
        }
        if number(&thread["comments"], "totalCount") > comments.len() as u64 {
            rows.push(Line::text(
                format!(
                    "Showing {} of {} comments · more on GitHub",
                    comments.len(),
                    number(&thread["comments"], "totalCount")
                ),
                Style::Muted,
            ));
        }
        let draft = workspace
            .drafts
            .get(&workspace.draft_key(&reply))
            .and_then(|fields| fields.first())
            .is_some_and(|text| !text.trim().is_empty());
        let mut button = Line::button(
            if draft {
                "[ Continue draft ]"
            } else {
                "[ Reply ]"
            },
            reply.clone(),
        );
        button.thread_action = true;
        rows.push(button);
        if let Some(resolved) = resolved {
            let mut button = Line::button(
                if resolved {
                    "[ Unresolve conversation ]"
                } else {
                    "[ Resolve conversation ]"
                },
                Action::Resolve(thread_id.into(), !resolved),
            );
            button.thread_action = true;
            rows.push(button);
        }
        if workspace.offers_commit(id, thread_id) {
            let mut button = Line::button(
                "[ Commit suggestion ]",
                Action::CommitSuggestion(id, thread_id.into()),
            );
            button.thread_action = true;
            rows.push(button);
        }
        for row in &mut rows[start..] {
            row.reply = Some(reply.clone());
        }
        rows.push(Line::text("", Style::Default));
    }
}
pub(super) fn detail_lines(workspace: &Workspace, width: usize) -> Vec<Line> {
    let mut rows = Vec::new();
    if workspace.repo.is_empty() {
        error_or_loading(workspace, &Key::Repository, &mut rows);
    } else if workspace.page < 3 {
        // Issues and pull requests paint on the shell; find walks the same rows.
        rows = super::paint::record_rows(workspace, width, workspace.icons);
    }
    // Keep even whitespace-only logical sources canonical across disclosure.
    let (mut primary, quiet): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|row| !row.quiet_review);
    primary.extend(quiet);
    primary
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace(page: usize) -> Workspace {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.page = page;
        workspace.repo = "example/demo".into();
        workspace.host = "github.com".into();
        let item = json!({
            "number": 7, "title": "Read the actual content before editing metadata",
            "state": "OPEN", "body": "The description comes first.",
            "author": {"login": "alice"}, "headRefName": "feature", "baseRefName": "main",
            "closedByPullRequestsReferences": [{"number": 9, "title": "Linked work"}],
            "closingIssuesReferences": [{"number": 9, "title": "Linked work"}],
            "mergeStateStatus": "BLOCKED", "files": [], "commits": []
        });
        for (key, value) in [
            (
                Key::Repository,
                json!({"nameWithOwner":"example/demo","url":"https://github.com/example/demo"}),
            ),
            (workspace.list_key(), json!([item.clone()])),
            (Key::Item(page, 7), item),
            (
                Key::Timeline(7),
                json!([[{
                    "event": "labeled", "actor": {"login": "alice"}, "label": {"name": "bug"}
                }, {
                    "event": "commented", "user": {"login": "bob"}, "body": "Read this discussion."
                }]]),
            ),
            (
                Key::Threads(7),
                json!({"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}),
            ),
        ] {
            workspace.cache.insert(
                key,
                Ok(Reply {
                    text: value.to_string(),
                    value,
                }),
            );
        }
        workspace
    }

    #[test]
    fn prose_uses_shared_semantics_and_clips_styles_to_lossless_logical_fragments() {
        let text = "### Title\n**before needle after e\u{301} 👩‍💻 and a long continuation**\n> quoted\n- item\n[docs](https://hidden.test)\n```rust\n\tlet literal = \"**keep**\";\n```\n| Name | Value |\n|---|---|\n| row | `cell` |";
        let mut styled = crate::ui::markdown::Styled::default();
        crate::ui::markdown::render_logical(&mut styled, text);
        for width in [10, 35, 100] {
            let mut rows = Vec::new();
            body(&mut rows, text, width);
            let logical = rows
                .iter()
                .filter_map(|row| {
                    row.source
                        .as_ref()
                        .filter(|source| source.offset == 0)
                        .map(|source| source.text.as_ref())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                logical,
                styled.text.split_terminator('\n').collect::<Vec<_>>()
            );
            let mut start = 0;
            for row in &rows {
                let source = row.source.as_ref().unwrap();
                if source.offset == 0 {
                    start = 0;
                }
                assert_eq!(source.offset, start);
                assert_eq!(&source.text[start..start + row.text.len()], row.text);
                for (range, _) in &row.spans {
                    assert!(row.text.get(range.clone()).is_some());
                }
                if source.text.starts_with("before needle") {
                    assert_eq!(row.spans, [(0..row.text.len(), Highlight::STRONG)]);
                }
                start += row.text.len();
                assert!(row.action.is_none() && row.gutter.is_empty());
            }
            assert!(rows.iter().any(|row| {
                row.spans
                    .iter()
                    .any(|(_, role)| *role == Highlight::HEADING)
            }));
            assert!(
                rows.iter()
                    .any(|row| row.spans.iter().any(|(_, role)| *role == Highlight::STRONG))
            );
            assert!(
                !logical
                    .iter()
                    .any(|line| line.contains("hidden.test") || line.contains("```"))
            );
        }
    }

    #[test]
    fn query_cache_is_scoped_and_filters_leave_linked_items() {
        let mut workspace = workspace(2);
        let all = workspace.list_key();
        workspace.opened = Some((2, 99));
        workspace
            .submit(
                Action::Filter,
                vec!["review-requested:@me".into(), "open".into()],
            )
            .unwrap();
        assert_ne!(all, workspace.list_key());
        assert!(workspace.items().is_empty());
        assert_eq!(workspace.opened, None);
        assert_eq!(workspace.focus, 0);
        assert!(workspace.cache.contains_key(&all));
        workspace.page = 1;
        workspace.save_draft(
            &Action::Filter,
            vec!["unsaved issue query".into(), "all".into()],
        );
        workspace.page = 2;
        let form = workspace.activate(Action::Filter).unwrap().unwrap();
        // The filter opens on the query with a trailing space, ready for another qualifier.
        assert_eq!(form.fields[0].input.input, "review-requested:@me ");
        assert!(form.fields[1].choices.contains(&"merged".into()));
    }

    #[test]
    fn reply_counts_distinguish_complete_partial_unknown_and_inconsistent_totals() {
        for (loaded, total, expected) in [
            (0, json!(4), None),
            (1, json!(1), None),
            (2, json!(2), Some("1 reply")),
            (3, json!(3), Some("2 replies")),
            (1, json!(2), Some("0 of 1 reply loaded")),
            (2, json!(5), Some("1 of 4 replies loaded")),
            (100, json!(101), Some("99 of 100 replies loaded")),
            (2, Value::Null, Some("1 reply loaded")),
            (3, json!("3"), Some("2 replies loaded")),
            (2, json!(1), Some("1 reply loaded")),
        ] {
            assert_eq!(
                thread_reply_count(&json!({"comments":{"totalCount":total}}), loaded),
                expected.map(str::to_owned)
            );
        }
        assert_eq!(
            thread_reply_count(&json!({"comments":{}}), 2).as_deref(),
            Some("1 reply loaded")
        );
    }

    #[test]
    fn partial_thread_headers_and_existing_loading_row_report_actual_loaded_counts() {
        let mut workspace = review_workspace();
        let comments = &mut workspace
            .cache
            .get_mut(&Key::Threads(7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value[0]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"];
        comments["totalCount"] = json!(5);
        comments["nodes"].as_array_mut().unwrap().push(json!({
            "author":{"login":"reply-author"},"body":"Loaded second comment",
            "path":"example.rs","line":null,"originalLine":42
        }));
        let cached = workspace.value(&Key::Threads(7)).unwrap().clone();
        let mut rows = Vec::new();
        threads(&workspace, 7, &mut rows, 59, Some("example.rs"));
        assert!(rows.iter().any(|row| row.thread_header
            && row.text == "◔ Unresolved · alice · original line 42 · 1 of 4 replies loaded"));
        assert_eq!(
            rows.iter()
                .filter(|row| row.text == "Showing 2 of 5 comments · more on GitHub")
                .count(),
            1
        );
        assert_eq!(workspace.value(&Key::Threads(7)).unwrap(), &cached);
        assert!(workspace.queue.is_empty() && workspace.active.idle());
    }

    #[test]
    fn files_headers_shorten_only_exact_root_paths_and_keep_thread_source_and_drafts() {
        for (selected, root_path, current, original, expected) in [
            (
                Some("example.rs"),
                "example.rs",
                json!(5),
                Value::Null,
                "◔ Unresolved · alice · line 5 · 1 reply",
            ),
            (
                Some("example.rs"),
                "example.rs",
                Value::Null,
                json!(42),
                "◔ Unresolved · alice · original line 42 · 1 reply",
            ),
            (
                None,
                "example.rs",
                json!(5),
                Value::Null,
                "◔ Unresolved · alice · example.rs:5 · 1 reply",
            ),
            (
                Some("example.rs"),
                "other.rs",
                json!(5),
                Value::Null,
                "◔ Unresolved · alice · other.rs:5 · 1 reply",
            ),
            (
                Some("example.rs"),
                "example.rs",
                Value::Null,
                Value::Null,
                "◔ Unresolved · alice · 1 reply",
            ),
        ] {
            let mut workspace = workspace(2);
            let value = json!([{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{
                "id":"THREAD_HEADER","isResolved":false,"comments":{"totalCount":2,"nodes":[
                    {"author":{"login":"alice"},"body":"Root 界 e\u{301} source bytes","path":root_path,
                        "line":current,"originalLine":original},
                    {"author":{"login":"bob"},"body":"Reply source bytes","path":"example.rs","line":5}
                ]}
            }]}}}}}]);
            workspace.cache.insert(
                Key::Threads(7),
                Ok(Reply {
                    text: value.to_string(),
                    value: value.clone(),
                }),
            );
            let reply = Action::Reply {
                number: 7,
                thread: "THREAD_HEADER".into(),
                repo: workspace.repo.clone(),
                host: workspace.host.clone(),
            };
            workspace.save_draft(&reply, vec!["Retained draft 界".into()]);
            let drafts = workspace.drafts.clone();
            let mut rows = Vec::new();
            threads(&workspace, 7, &mut rows, 59, selected);
            let header = rows.iter().find(|row| row.thread_header).unwrap();
            assert_eq!(header.text, expected);
            assert_eq!(header.reply.as_ref(), Some(&reply));
            assert!(header.action.is_none());
            assert!(rows.iter().any(|row| {
                row.source_scope == Some(SourceScope::Thread("THREAD_HEADER".into(), 0, 1))
                    && row.source.as_ref().is_some_and(|source| {
                        source.text.as_ref() == "Root 界 e\u{301} source bytes"
                    })
            }));
            assert!(
                rows.iter().any(
                    |row| row.text == "› Continue draft" && row.action.as_ref() == Some(&reply)
                )
            );
            if expected.contains("original line") {
                assert!(
                    rows.iter()
                        .any(|row| row.text == "Outdated · original line")
                );
            }
            assert_eq!(workspace.value(&Key::Threads(7)).unwrap(), &value);
            assert_eq!(workspace.drafts, drafts);
            assert!(workspace.queue.is_empty() && workspace.active.idle());
        }
    }

    fn review_workspace() -> Workspace {
        let mut workspace = workspace(2);
        // Review threads read in the Conversation (and beside their lines in Files).
        workspace.pr_tab = super::super::pr::CONVERSATION;
        let pages = json!([
            {"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[
                    {"author":{"login":"alice"},"body":"Original discussion", "path":"example.rs","line":null,"originalLine":42}
                ]}}
            ]}}}}},
            {"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                {"id":"THREAD_B","isResolved":true,"comments":{"nodes":[
                    {"author":{"login":"bob"},"body":"Second discussion", "path":"example.rs","line":1}
                ]}}
            ]}}}}}
        ]);
        workspace.cache.insert(
            Key::Threads(7),
            Ok(Reply {
                text: pages.to_string(),
                value: pages,
            }),
        );
        workspace
    }

    #[test]
    fn neutral_thread_rows_keep_separator_and_unscoped_fallback_boundaries() {
        fn thread_rows(reply: &Action, author: &str) -> Vec<Line> {
            let mut header = Line::text("Neutral thread header", Style::Muted);
            header.thread_header = true;
            header.reply = Some(reply.clone());
            let mut author = Line::text(author, Style::Muted);
            author.reply = Some(reply.clone());
            let mut button = Line::button("Reply", reply.clone());
            button.thread_action = true;
            button.reply = Some(reply.clone());
            vec![header, author, button, Line::text("", Style::Default)]
        }
        fn position(rows: &[Line], cursor: usize) -> ReadingPosition {
            let mut position = ReadingPosition {
                context: Some("same parent".into()),
                ..ReadingPosition::default()
            };
            position.finish_scoped(rows, cursor, 0, (0..rows.len()).collect());
            position
        }
        let reply = |thread: &str| Action::Reply {
            number: 7,
            thread: thread.into(),
            repo: "example/demo".into(),
            host: "github.com".into(),
        };
        let a = reply("THREAD_A");
        let b = reply("THREAD_B");

        let mut author_blank = thread_rows(&a, "  ↳ same-author");
        author_blank.insert(2, Line::text("", Style::Default));
        assert!(
            neutral_thread_row(&author_blank, 2).is_none(),
            "an arbitrary blank after an author is not the thread separator"
        );

        let old_root = vec![Line::text("", Style::Default)];
        let mut scoped_then_root = thread_rows(&a, "  ↳ same-author");
        scoped_then_root.push(Line::text("Unscoped metadata", Style::Muted));
        scoped_then_root.push(Line::text("", Style::Default));
        let mut cursor = 0;
        position(&old_root, cursor).restore_scoped(
            "same parent".into(),
            &old_root,
            &scoped_then_root,
            &mut cursor,
            0,
        );
        assert_eq!(cursor, 5, "an unscoped blank must not select A's separator");

        let mut old = thread_rows(&a, "  ↳ same-author");
        old.extend(thread_rows(&a, "unused").into_iter().skip(2));
        old.extend(thread_rows(&b, "  ↳ same-author"));
        let mut shifted = vec![Line::text("Changed unscoped chrome", Style::Muted)];
        shifted.extend(thread_rows(&a, "  ↳ same-author"));
        shifted.extend(thread_rows(&a, "unused").into_iter().skip(2));
        shifted.extend(thread_rows(&b, "  ↳ same-author"));
        let mut cursor = 5; // The second identical empty separator within A.
        position(&old, cursor).restore_scoped("same parent".into(), &old, &shifted, &mut cursor, 0);
        assert_eq!(
            cursor, 6,
            "same-Reply duplicate separators need occurrence identity"
        );
        assert_eq!(neutral_thread_row(&shifted, cursor).unwrap().0, &a);

        let mut changed = thread_rows(&a, "  ↳ replacement-author");
        changed.extend(thread_rows(&b, "  ↳ same-author"));
        let mut cursor = 1;
        position(&old, cursor).restore_scoped("same parent".into(), &old, &changed, &mut cursor, 0);
        assert!(changed[cursor].thread_header && changed[cursor].action.is_none());
        assert_eq!(
            changed[cursor].reply.as_ref(),
            Some(&a),
            "a replaced A neutral row must not select B's identical author"
        );

        let mut removed = vec![Line::text("Neutral parent", Style::Muted)];
        removed.extend(thread_rows(&b, "  ↳ same-author"));
        let mut cursor = 5;
        position(&old, cursor).restore_scoped("same parent".into(), &old, &removed, &mut cursor, 0);
        assert_eq!(
            cursor, 0,
            "a removed scoped separator must not select B's blank"
        );
        assert!(removed[cursor].reply.is_none() && removed[cursor].action.is_none());
    }

    #[test]
    fn compact_native_jk_traverses_scoped_authors_replies_and_duplicate_separators() {
        use crate::ui::{KeyInputKind, UiEvent};

        let root = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let mut app = crate::git::screen::GitScreen::open(root.path()).unwrap();
        let mut workspace = files_workspace();
        workspace.focus = 2;
        workspace.root = root.path().to_path_buf();
        let root_body = "Wrapped root 界 e\u{301} 👩‍💻 must retain every source byte while native navigation crosses the reply author. ".repeat(2);
        let comment = |body: &str| {
            json!({
                "author":{"login":"same-author"},"body":body,"path":"example.rs","line":1
            })
        };
        let threads = json!({"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"THREAD_A","isResolved":false,"comments":{"nodes":[
                comment(&root_body), comment("Identical reply body 界"), comment("Identical reply body 界")
            ]}},
            {"id":"THREAD_B","isResolved":false,"comments":{"nodes":[
                comment("Next thread body 界"), comment("Identical reply body 界")
            ]}}
        ]}}}}});
        workspace.cache.insert(
            Key::Threads(7),
            Ok(Reply {
                text: threads.to_string(),
                value: threads.clone(),
            }),
        );
        workspace.cache.insert(
            Key::Viewed(7),
            Ok(Reply {
                text: String::new(),
                value: json!([{"data":{"repository":{"pullRequest":{"files":{"nodes":[
                    {"path":"example.rs","viewerViewedState":"UNVIEWED"}
                ]}}}}}]),
            }),
        );
        for thread in ["THREAD_A", "THREAD_B"] {
            let reply = Action::Reply {
                number: 7,
                thread: thread.into(),
                repo: workspace.repo.clone(),
                host: workspace.host.clone(),
            };
            workspace.save_draft(&reply, vec![format!("Unchanged draft {thread} 界")]);
        }
        let drafts = workspace.drafts.clone();
        let patch = workspace.raw(&Key::Diff(7)).unwrap().to_owned();
        app.page = 2;
        app.github = workspace;
        app.frame(64, 20);
        let first = app
            .github
            .detail_rows
            .iter()
            .position(|row| {
                row.source_scope == Some(SourceScope::Thread("THREAD_A".into(), 0, 1))
                    && row.source.as_ref().is_some_and(|source| source.offset == 0)
            })
            .unwrap();
        let last = (0..app.github.detail_rows.len())
            .find(|&index| {
                let row = &app.github.detail_rows[index];
                row.text.is_empty()
                    && row.source.is_none()
                    && row.reply.is_none()
                    && neutral_thread_row(&app.github.detail_rows, index).is_some_and(
                        |(reply, _, _)| {
                            matches!(reply,
                        Action::Reply { thread, .. } if thread == "THREAD_B")
                        },
                    )
            })
            .unwrap();
        app.github.detail_cursor = first;
        app.frame(64, 20);
        let expected = app.github.detail_rows[first..=last]
            .iter()
            .map(|row| {
                (
                    row.text.clone(),
                    row.source_scope.clone(),
                    row.source
                        .as_ref()
                        .map(|source| (source.text.clone(), source.offset)),
                    row.action.clone(),
                    row.reply.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            expected.iter().any(|(_, scope, source, _, _)| {
                *scope == Some(SourceScope::Thread("THREAD_A".into(), 0, 1))
                    && source.as_ref().is_some_and(|(_, offset)| *offset > 0)
            }),
            "fixture must genuinely wrap the root comment"
        );
        assert_eq!(
            expected
                .iter()
                .filter(|row| row.0 == "  ↳ same-author")
                .count(),
            3
        );
        assert_eq!(
            expected
                .iter()
                .filter(|row| row.0.is_empty() && row.2.is_none())
                .count(),
            2
        );

        // Every actual native key is followed by the production frame, including
        // authors, repeated separators, controls and positive-offset fragments.
        for (code, indices) in [
            ('j', (first + 1..=last).collect::<Vec<_>>()),
            ('k', (first..last).rev().collect::<Vec<_>>()),
        ] {
            for index in indices {
                app.event(UiEvent::Key {
                    key: KeyEvent::plain(KeyCode::Char(code)),
                    kind: KeyInputKind::Press,
                });
                let rendered = app.frame(64, 20);
                assert_eq!(
                    app.github.detail_cursor,
                    index,
                    "{}",
                    rendered.grid.row_text(2)
                );
                let row = &app.github.detail_rows[index];
                let actual = (
                    row.text.clone(),
                    row.source_scope.clone(),
                    row.source
                        .as_ref()
                        .map(|source| (source.text.clone(), source.offset)),
                    row.action.clone(),
                    row.reply.clone(),
                );
                assert_eq!(actual, expected[index - first]);
                let (x, y, _) = rendered
                    .cursor
                    .expect("each canonical row needs a native cursor");
                assert!(x < 24 && (7..17).contains(&y));
                assert_eq!(app.github.drafts, drafts);
                assert_eq!(app.github.value(&Key::Threads(7)).unwrap(), &threads);
                assert_eq!(app.github.raw(&Key::Diff(7)).unwrap(), patch);
                assert!(app.github.queue.is_empty() && app.github.active.idle());
                assert!(!app.github.detail_find.search.active());
                assert!(app.form.is_none());
            }
        }
    }

    #[test]
    fn replies_preserve_paginated_thread_identity_and_outdated_line_context() {
        let mut workspace = review_workspace();
        let mut rows = Vec::new();
        threads(&workspace, 7, &mut rows, 70, Some("example.rs"));
        assert!(
            rows.iter()
                .any(|row| row.thread_header && row.text.contains("alice · original line 42"))
        );
        assert!(rows.iter().any(|row| row.text.contains("Outdated")));
        let index = rows
            .iter()
            .position(|row| row.text == "Second discussion")
            .unwrap();
        workspace.focus = 2;
        workspace.detail_cursor = index;
        workspace.detail_rows = rows;
        let form = workspace
            .event(&KeyEvent::plain(KeyCode::Char('c')), None)
            .unwrap()
            .unwrap();
        let FormAction::Github(action) = form.action else {
            panic!("native reply form required")
        };
        assert!(
            matches!(&action, Action::Reply { number: 7, thread, repo, host } if thread == "THREAD_B" && repo == "example/demo" && host == "github.com")
        );
        assert!(
            action.writes(),
            "reply must pass the normal write authorization gate"
        );
        workspace
            .submit(action, vec!["Reply to the resolved original thread".into()])
            .unwrap();
        let request = workspace.queue.back().unwrap();
        assert_eq!(request.key, Key::Write);
        let Op::Write(target, write) = &request.op else {
            panic!("a reply is a write")
        };
        assert_eq!(
            target,
            &ForgeTarget::repository("github.com", "example/demo")
        );
        assert_eq!(
            write,
            &Write::Reply {
                number: 7,
                thread: "THREAD_B".into(),
                body: "Reply to the resolved original thread".into(),
            },
            "a reply names only its thread, never a path, line or commit"
        );
        assert_eq!(
            workspace.review_threads(7)[1]["isResolved"],
            json!(true),
            "replying must not reopen a resolved thread"
        );
    }

    #[test]
    fn reply_drafts_are_scoped_to_host_repository_pr_and_thread_and_reject_stale_sources() {
        let mut workspace = review_workspace();
        let action = Action::Reply {
            number: 7,
            thread: "THREAD_A".into(),
            repo: workspace.repo.clone(),
            host: workspace.host.clone(),
        };
        workspace.save_draft(
            &action,
            vec![
                "My original thread draft".into(),
                "Stale source text must never override identity".into(),
            ],
        );
        let restored = workspace.activate(action.clone()).unwrap().unwrap();
        assert_eq!(restored.fields[0].input.input, "My original thread draft");
        assert!(restored.fields[1].read_only);
        assert_eq!(
            restored.fields[1].input.input,
            "github.com/example/demo · PR #7"
        );
        for other in [
            Action::Reply {
                number: 7,
                thread: "THREAD_B".into(),
                repo: workspace.repo.clone(),
                host: workspace.host.clone(),
            },
            Action::Reply {
                number: 8,
                thread: "THREAD_A".into(),
                repo: workspace.repo.clone(),
                host: workspace.host.clone(),
            },
            Action::Reply {
                number: 7,
                thread: "THREAD_A".into(),
                repo: "other/repository".into(),
                host: workspace.host.clone(),
            },
            Action::Reply {
                number: 7,
                thread: "THREAD_A".into(),
                repo: workspace.repo.clone(),
                host: "github.enterprise.test".into(),
            },
        ] {
            let form = workspace.form(
                "Other reply",
                other,
                vec![actions::field("Reply", "", true, true)],
            );
            assert!(
                form.fields[0].input.input.is_empty(),
                "draft must not cross source or thread identity"
            );
        }
        workspace.repo = "other/repository".into();
        assert!(
            workspace
                .submit(
                    action.clone(),
                    vec!["Edited before the source changed".into()]
                )
                .is_err()
        );
        assert!(
            workspace.queue.is_empty(),
            "a stale source must never enqueue a mutation"
        );
        workspace.repo = "example/demo".into();
        let restored = workspace.activate(action).unwrap().unwrap();
        assert_eq!(
            restored.fields[0].input.input,
            "Edited before the source changed"
        );
        workspace.save_draft(&Action::Comment(2, 7), vec!["Repository comment".into()]);
        workspace.repo = "other/repository".into();
        assert!(
            workspace
                .activate(Action::Comment(2, 7))
                .unwrap()
                .unwrap()
                .fields[0]
                .input
                .input
                .is_empty()
        );
    }

    #[test]
    fn scoped_resolve_refresh_never_selects_opposite_or_another_thread_and_keeps_drafts() {
        let mut workspace = review_workspace();
        workspace.focus = 2;
        let reply = Action::Reply {
            number: 7,
            thread: "THREAD_A".into(),
            repo: workspace.repo.clone(),
            host: workspace.host.clone(),
        };
        workspace.save_draft(&reply, vec!["Exact draft A\n界".into()]);
        let drafts = workspace.drafts.clone();
        let old = detail_lines(&workspace, 35);
        let mut cursor = old
            .iter()
            .position(|row| row.action == Some(Action::Resolve("THREAD_A".into(), true)))
            .unwrap();
        let context = position_context(&workspace);
        let mut position = ReadingPosition {
            context: Some(context.clone()),
            ..ReadingPosition::default()
        };
        position.finish_scoped(&old, cursor, 0, (0..old.len()).collect());
        workspace
            .cache
            .get_mut(&Key::Threads(7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value[0]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["isResolved"] =
            json!(true);
        let updated = detail_lines(&workspace, 70);
        position.restore_scoped(context.clone(), &old, &updated, &mut cursor, 0);
        assert!(updated[cursor].thread_header);
        assert_eq!(updated[cursor].reply.as_ref(), Some(&reply));
        assert!(
            !matches!(updated[cursor].action, Some(Action::Resolve(..))),
            "refresh must not arm Reopen or another Resolve"
        );
        // A resolved thread folds; opened, its reply still continues the draft.
        let fold = updated[cursor]
            .action
            .clone()
            .filter(|action| matches!(action, Action::Collapse(..)));
        if let Some(fold) = fold.clone() {
            workspace.activate(fold).unwrap();
        }
        assert!(
            detail_lines(&workspace, 70)
                .iter()
                .any(|row| row.action.as_ref() == Some(&reply)
                    && row.text.contains("Continue draft"))
        );
        if let Some(fold) = fold {
            workspace.activate(fold).unwrap();
        }
        position.finish_scoped(&updated, cursor, 0, (0..updated.len()).collect());
        workspace
            .cache
            .get_mut(&Key::Threads(7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value[0]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
        let removed = detail_lines(&workspace, 35);
        position.restore_scoped(context, &updated, &removed, &mut cursor, 0);
        assert!(removed[cursor].action.is_none());
        assert!(
            removed[cursor].reply.is_none(),
            "removed A must not retarget B"
        );
        assert_eq!(workspace.drafts, drafts);
        assert!(workspace.queue.is_empty());
    }

    #[test]
    fn scoped_thread_byte_anchor_roundtrips_without_matching_identical_other_thread() {
        let mut workspace = review_workspace();
        let text = "prefix 界 e\u{301} 👩‍💻 repeated words keep byte offsets through wide and narrow reflow ".repeat(3);
        for page in [0, 1] {
            workspace
                .cache
                .get_mut(&Key::Threads(7))
                .unwrap()
                .as_mut()
                .unwrap()
                .value[page]["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]
                ["nodes"][0]["body"] = json!(text);
        }
        // THREAD_B is resolved, so it starts folded; open it.
        workspace
            .collapsed
            .insert((7, super::super::conversation::thread_key("THREAD_B")));
        let narrow = detail_lines(&workspace, 20);
        let mut cursor = narrow
            .iter()
            .position(|row| {
                matches!(&row.source_scope,
            Some(SourceScope::Thread(thread, 0, 1)) if thread == "THREAD_B")
                    && row.source.as_ref().is_some_and(|source| source.offset > 40)
            })
            .unwrap();
        let byte = narrow[cursor].source.as_ref().unwrap().offset;
        let context = position_context(&workspace);
        let mut position = ReadingPosition {
            context: Some(context.clone()),
            ..ReadingPosition::default()
        };
        position.finish_scoped(&narrow, cursor, 0, (0..narrow.len()).collect());
        let wide = detail_lines(&workspace, 100);
        position.restore_scoped(context.clone(), &narrow, &wide, &mut cursor, 0);
        assert!(
            matches!(&wide[cursor].source_scope, Some(SourceScope::Thread(thread, 0, 1)) if thread == "THREAD_B")
        );
        position.finish_scoped(&wide, cursor, 0, (0..wide.len()).collect());
        position.restore_scoped(context, &wide, &narrow, &mut cursor, 0);
        assert_eq!(narrow[cursor].source.as_ref().unwrap().offset, byte);
    }

    #[test]
    fn issue_and_pr_status_ranges_never_cover_last_payload_or_important_status() {
        use crate::ui::{KeyInputKind, UiEvent};

        fn literal_workspace(page: usize, files: bool, code: bool, literal: &str) -> Workspace {
            let mut workspace = if files {
                files_workspace()
            } else {
                workspace(page)
            };
            workspace.focus = 2;
            let lines = (0..80).map(|_| format!("{literal}\n")).collect::<String>();
            if files {
                workspace
                    .cache
                    .get_mut(&Key::Diff(7))
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .text = format!(
                    "diff --git a/example.rs b/example.rs\n--- /dev/null\n+++ b/example.rs\n@@ -0,0 +1,80 @@\n{}",
                    (0..80).map(|_| format!("+{literal}\n")).collect::<String>()
                );
                workspace.cache.insert(
                    Key::Viewed(7),
                    Ok(Reply {
                        text: String::new(),
                        value: json!([{"data":{"repository":{"pullRequest":{"files":{"nodes":[
                            {"path":"example.rs","viewerViewedState":"UNVIEWED"}
                        ]}}}}}]),
                    }),
                );
            } else {
                workspace
                    .cache
                    .get_mut(&Key::Item(page, 7))
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .value["body"] = json!(if code {
                    format!("```text\n{lines}```\n")
                } else {
                    lines
                });
            }
            workspace.cache.insert(
                Key::Timeline(7),
                Ok(Reply {
                    text: String::new(),
                    value: json!([]),
                }),
            );
            workspace
        }
        fn press(app: &mut crate::git::screen::GitScreen, code: KeyCode) {
            app.event(UiEvent::Key {
                key: KeyEvent::plain(code),
                kind: KeyInputKind::Press,
            });
        }
        let root = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let mut app = crate::git::screen::GitScreen::open(root.path()).unwrap();
        for (page, files, code) in [
            (1, false, false),
            (1, false, true),
            (2, false, false),
            (2, false, true),
            (2, true, true),
        ] {
            for (width, height) in [(64_u16, 20_u16), (100, 30), (190, 48)] {
                app.page = page;
                app.github = literal_workspace(page, files, code, "PROBE");
                app.frame(width, height);
                app.github.detail_cursor = app
                    .github
                    .detail_rows
                    .iter()
                    .position(|row| {
                        row.source
                            .as_ref()
                            .is_some_and(|source| source.text.as_ref() == "PROBE")
                    })
                    .unwrap();
                let probe = app.frame(width, height);
                let (x, _, _) = probe.cursor.unwrap();
                // Beside the Conversation's sidebar the text ends before it.
                let side = if app.github.detail_rows.iter().any(|row| row.side) {
                    super::super::conversation::sidebar_width(app.github.detail_width) + 3
                } else {
                    0
                };
                let available = usize::from(width - x - 2) - side;
                let length = if code {
                    available
                } else {
                    available.min(app.github.detail_width.min(80).saturating_sub(3))
                };
                let literal = format!("{}RIGHTEDGE", "x".repeat(length - 9));
                app.github = literal_workspace(page, files, code, &literal);
                app.frame(width, height);
                let first = app
                    .github
                    .detail_rows
                    .iter()
                    .position(|row| {
                        row.source
                            .as_ref()
                            .is_some_and(|source| source.text.as_ref() == literal)
                    })
                    .unwrap();
                app.github.detail_cursor = first;
                let rendered = app.frame(width, height);
                let (native_x, native_y, _) = rendered.cursor.unwrap();
                assert_eq!(native_x, x);
                // The pane's last body row sits above its border and the footer; status
                // never takes a payload row.
                let body = height - 3;
                let footer = height - 1;
                assert!(
                    rendered.grid.row_text(body).contains("RIGHTEDGE"),
                    "last literal covered at {width}, page {page}: {}",
                    rendered.grid.row_text(body)
                );
                assert_eq!(
                    (native_y..=body)
                        .filter(|&y| rendered.grid.row_text(y).contains("RIGHTEDGE"))
                        .count(),
                    usize::from(body + 1 - native_y),
                    "payload row lost"
                );
                let item = app.github.value(&Key::Item(page, 7)).unwrap().clone();
                let patch = app.github.raw(&Key::Diff(7)).map(str::to_owned);

                for status in [
                    String::from("Important short notice"),
                    "Important status ".repeat(30),
                ] {
                    app.github.notice = status.clone();
                    let shown = app.frame(width, height);
                    let row = shown.grid.row_text(footer);
                    // A notice that shares the row keeps the hints on the left.
                    if status.len() + 10 <= usize::from(width) - 2 {
                        assert!(
                            row.trim_end().ends_with(status.trim()) && row.contains("? keys"),
                            "{row}"
                        );
                    } else if status.len() < usize::from(width) - 2 {
                        assert_eq!(row.trim(), status.trim());
                    } else {
                        assert!(
                            row.trim().starts_with("Important status") && row.trim().ends_with('…'),
                            "{row}"
                        );
                    }
                    assert!(shown.grid.row_text(body).contains("RIGHTEDGE"));
                    assert_eq!(
                        shown.cursor.map(|(x, y, _)| (x, y)),
                        Some((native_x, native_y))
                    );
                }
                app.github.notice = "Workspace error has priority".into();
                app.github.notice_error = true;
                assert!(
                    app.frame(width, height)
                        .grid
                        .row_text(footer)
                        .trim_end()
                        .ends_with("✕ Workspace error has priority"),
                    "{}",
                    app.frame(width, height).grid.row_text(footer)
                );
                app.github.notice.clear();
                app.github.notice_error = false;
                app.notice = "Application error has priority".into();
                assert!(
                    app.frame(width, height)
                        .grid
                        .row_text(footer)
                        .trim_end()
                        .ends_with("✕ Application error has priority"),
                    "{}",
                    app.frame(width, height).grid.row_text(footer)
                );
                app.notice.clear();

                app.github.queue.push_back(Request::placeholder(Key::Write));
                assert!(
                    app.frame(width, height)
                        .grid
                        .row_text(footer)
                        .contains("Sending…")
                );
                app.github.queue.pop_back();
                let list_key = app.github.list_key();
                app.github.queue.push_back(Request::placeholder(list_key));
                if width >= 190 {
                    let row = app.frame(width, height).grid.row_text(footer);
                    assert!(row.contains("Refreshing…"), "{row}");
                }
                app.github.queue.pop_back();

                let mut find_key = KeyEvent::plain(KeyCode::Char('f'));
                find_key.modifiers.control = true;
                app.event(UiEvent::Key {
                    key: find_key,
                    kind: KeyInputKind::Press,
                });
                app.event(UiEvent::Paste("RIGHTEDGE".into()));
                press(&mut app, KeyCode::Enter);
                let found = app.frame(width, height);
                let find_status = app
                    .github
                    .detail_find
                    .footer_status(app.github.detail_cursor, width);
                let position = find_status.rsplit(" · ").next().unwrap();
                assert!(
                    found.grid.row_text(height - 1).contains(position),
                    "{find_status}"
                );
                let (x, y, _) = found.cursor.unwrap();
                assert_eq!(found.grid.get(x, y).unwrap().symbol, "R");
                assert_eq!(found.grid.get(x, y).unwrap().style, Style::Search);
                let row = &app.github.detail_rows[app.github.detail_cursor];
                assert_eq!(
                    app.github.detail_find.cursor_byte(app.github.detail_cursor),
                    Some(literal.find("RIGHTEDGE").unwrap() - row.source.as_ref().unwrap().offset)
                );
                assert_eq!(app.github.value(&Key::Item(page, 7)).unwrap(), &item);
                assert_eq!(app.github.raw(&Key::Diff(7)).map(str::to_owned), patch);
                assert!(app.github.queue.is_empty() && app.github.active.idle());
                assert!(app.form.is_none());
                app.github.detail_find = Default::default();
            }
        }
    }

    fn files_workspace() -> Workspace {
        let mut workspace = workspace(2);
        workspace.pr_tab = 3;
        workspace
            .cache
            .get_mut(&Key::Item(2, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["files"] = json!([
            {"path":"example.rs","additions":1,"deletions":1},
            {"path":"src/nested/other.rs","additions":1,"deletions":0}
        ]);
        workspace.cache.insert(Key::Diff(7), Ok(Reply { value: Value::Null, text: "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\n".into() }));
        workspace
    }

    #[test]
    fn malformed_or_issue_merged_filters_do_not_replace_the_active_query() {
        let mut workspace = workspace(1);
        let key = workspace.list_key();
        for values in [
            vec!["missing state".into()],
            vec!["bad state".into(), "merged".into()],
        ] {
            assert!(workspace.submit(Action::Filter, values).is_err());
            assert_eq!(workspace.list_key(), key);
            assert!(workspace.queue.is_empty());
        }
    }

    #[test]
    fn comment_key_uses_the_real_selected_diff_line_and_its_draft() {
        let mut workspace = workspace(2);
        let action = Action::Inline {
            number: 7,
            path: "src/deep/file.rs".into(),
            line: 42,
            side: "LEFT",
            head: "a".repeat(40),
        };
        workspace.focus = 2;
        workspace.detail_rows = vec![Line::button("removed source", action.clone())];
        workspace.save_draft(&action, vec!["Keep my inline draft".into()]);
        let form = workspace
            .event(&KeyEvent::plain(KeyCode::Char('c')), None)
            .unwrap()
            .unwrap();
        assert!(matches!(form.action, FormAction::Github(ref actual) if actual == &action));
        assert_eq!(form.fields[0].input.input, "Keep my inline draft");
        // In the list `c` checks out; `C` comments on the item.
        workspace.focus = 0;
        let form = workspace
            .event(&KeyEvent::plain(KeyCode::Char('C')), None)
            .unwrap()
            .unwrap();
        assert!(matches!(
            form.action,
            FormAction::Github(Action::Comment(2, 7))
        ));
    }

    #[test]
    fn files_keep_every_changed_line_with_a_thread_after_its_line() {
        // GH-*-03: a blank context line without its leading space, then the changes, and a
        // thread on new line 4: the whole hunk renders and the thread follows line 4.
        let mut workspace = files_workspace();
        workspace.cache.insert(
            Key::Diff(7),
            Ok(Reply {
                value: Value::Null,
                text: "diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1,4 +1,5 @@\n fn head() {}\n\n-fn before() {}\n+fn after() {}\n+fn extra() {}\n fn tail() {}\n".into(),
            }),
        );
        let value = json!([{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{
            "id":"THREAD_LINE","isResolved":true,"comments":{"nodes":[
                {"author":{"login":"alice"},"body":"Thread on the extra line","path":"example.rs","line":4}
            ]}
        }]}}}}}]);
        workspace.cache.insert(
            Key::Threads(7),
            Ok(Reply {
                text: value.to_string(),
                value,
            }),
        );
        for width in [61, 69, 120] {
            let rows = super::super::paint::record_rows(&workspace, width, workspace.icons);
            let at = |text: &str| {
                rows.iter()
                    .position(|row| row.text.contains(text))
                    .unwrap_or_else(|| panic!("{width}: no {text}"))
            };
            assert!(rows[at("fn before()")].diff == Some(false));
            assert!(rows[at("fn after()")].diff == Some(true));
            let extra = at("fn extra()");
            assert!(rows[extra].diff == Some(true));
            let thread = rows
                .iter()
                .position(|row| row.thread_header)
                .expect("thread");
            assert!(
                thread > extra && thread < at("fn tail()"),
                "{width}: the thread sits after line 4"
            );
        }
    }

    #[test]
    fn k_then_enter_beside_the_commit_list_opens_the_previous_commit() {
        // The Commits tab at 120 columns: the newest commit opens beside the list with the
        // cursor on its row; `k` moves to the commit before it and Enter opens that one.
        use crate::ui::{KeyInputKind, UiEvent};

        let root = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let mut app = crate::git::screen::GitScreen::open(root.path()).unwrap();
        let mut workspace = workspace(2);
        workspace.root = root.path().to_path_buf();
        let shas = ["a".repeat(40), "b".repeat(40), "c".repeat(40)];
        workspace
            .cache
            .get_mut(&Key::Item(2, 7))
            .unwrap()
            .as_mut()
            .unwrap()
            .value["commits"] = json!(
            shas.iter()
                .enumerate()
                .map(|(index, sha)| json!({
                    "oid": sha, "messageHeadline": format!("Commit number {index}"),
                    "committedDate": "2026-11-08T10:00:00Z"
                }))
                .collect::<Vec<_>>()
        );
        for (index, sha) in shas.iter().enumerate() {
            workspace.cache.insert(
                Key::Commit(sha.clone()),
                Ok(Reply {
                    value: Value::Null,
                    text: format!("diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-old {index}\n+new {index}\n"),
                }),
            );
        }
        workspace.focus = 2;
        app.page = 2;
        app.github = workspace;
        app.github
            .activate(Action::PrTab(super::super::pr::COMMITS))
            .unwrap();
        app.frame(120, 36);
        app.frame(120, 36);
        let open_row = |app: &crate::git::screen::GitScreen| {
            let row = &app.github.detail_rows[app.github.detail_cursor];
            (row.outline, row.action.clone())
        };
        assert_eq!(
            app.github.selected_commit.as_deref(),
            Some(shas[2].as_str()),
            "the newest opens"
        );
        assert_eq!(
            open_row(&app),
            (false, Some(Action::Commit(shas[2].clone())))
        );
        for code in [KeyCode::Char('k'), KeyCode::Enter] {
            app.event(UiEvent::Key {
                key: KeyEvent::plain(code),
                kind: KeyInputKind::Press,
            });
            app.frame(120, 36);
        }
        app.frame(120, 36);
        assert_eq!(
            app.github.selected_commit.as_deref(),
            Some(shas[1].as_str()),
            "k then Enter opens the one before"
        );
        assert_eq!(
            open_row(&app),
            (false, Some(Action::Commit(shas[1].clone())))
        );
        assert!(
            app.github
                .detail_rows
                .iter()
                .any(|row| !row.outline && row.text.contains("Commit number 1"))
        );
        assert!(
            app.github
                .detail_rows
                .iter()
                .any(|row| row.text.contains("new 1"))
        );
    }
}
