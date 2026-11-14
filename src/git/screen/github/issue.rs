//! F-ISSUES / F-ISSUEDISC reader rows (DESIGN.md §3.33, §3.34): the Overview metadata table
//! and body, the Discussion comment cards and the Activity rail, built from typed records.
use super::super::shell::glyphs::{Glyph, glyph};
use super::prose::{self, Rich, blank, field, heading, muted, role, rule, strong};
use super::lines::Line;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::records::{self, CommentRecord, DetailRecord, LinkRecord, TimelineEntry};
use crate::ui::{GitRole, GitSurface};

pub(super) const TABS: [&str; 3] = ["Overview", "Discussion", "Activity"];

/// Opens a linked issue or pull request in place (or in its own repository from the inbox).
pub(super) fn link_action(workspace: &Workspace, link: &LinkRecord, page: usize) -> Option<Action> {
    if workspace.inbox.enabled || !link.repo.is_empty() && !link.repo.eq_ignore_ascii_case(&workspace.repo) {
        let target = inbox::Target::from_url(&link.url, &workspace.host, page, link.number).ok()?;
        return Some(Action::OpenRemote(target));
    }
    Some(Action::OpenItem(page, link.number))
}

/// `⎇ #11 Recover queued tasks after worker reconnect` as a link row.
pub(super) fn link_row(
    workspace: &Workspace,
    link: &LinkRecord,
    pull: bool,
    lead: Rich,
    mode: ExplorerIconMode,
) -> Line {
    let page = if pull { 2 } else { 1 };
    let mark = glyph(if pull { Glyph::PullRequest } else { Glyph::Issue }, mode);
    let mut rich = lead.push(&format!("{mark} "), role(GitRole::Ref));
    if !link.repo.is_empty() && !link.repo.eq_ignore_ascii_case(&workspace.repo) {
        rich = rich.push(&format!("{} ", link.repo), role(GitRole::Muted));
    }
    let mut row = rich
        .push(&format!("{}{} ", workspace.sigil(pull), link.number), role(GitRole::Link))
        .push(&link.title, role(GitRole::Link))
        .line();
    row.action = link_action(workspace, link, page);
    row
}

/// A detail read that failed: what is shown instead, why, and how to fix it (§3.45).
pub(super) fn unavailable(workspace: &Workspace, rows: &mut Vec<Line>, error: &str, refresh: &str) {
    // A banner above the body (§3.45): the failure and the way out on a tinted band, then
    // the list's description underneath, untouched.
    let mut head = Rich::new()
        .plain(" ")
        .push("✕ ", strong(GitRole::Removed))
        .push("Details unavailable", strong(GitRole::Removed))
        .sep()
        .push(error, role(GitRole::Text))
        .line();
    head.band = Some(GitSurface::ReceiptErr);
    rows.push(head);
    let (command, token) = sign_in(workspace.forge_kind());
    let mut hint = Rich::new()
        .plain("   ")
        .push(refresh, strong(GitRole::Focus))
        .push(" retry · signed out? run ", role(GitRole::Text))
        .push(command, strong(GitRole::Text))
        .push(" or set ", role(GitRole::Text))
        .push(token, strong(GitRole::Text))
        .push(" · below: the list's description", role(GitRole::Muted))
        .line();
    hint.band = Some(GitSurface::ReceiptErr);
    rows.push(hint);
}

/// How this forge's own CLI signs in, and the token variable Dusty reads instead (§3.45).
pub(super) fn sign_in(kind: Option<crate::forge::provider::ProviderKind>) -> (&'static str, &'static str) {
    use crate::forge::provider::ProviderKind;
    match kind {
        Some(ProviderKind::GitLab) => ("glab auth login", "GITLAB_TOKEN"),
        Some(ProviderKind::Gitea) => ("tea login add", "GITEA_TOKEN"),
        Some(ProviderKind::Forgejo) => ("fj auth login", "FORGEJO_TOKEN"),
        Some(ProviderKind::GitHub) | None => ("gh auth login", "GH_TOKEN"),
    }
}

/// `󰀄 alex · 2h ago` over a comment.
pub(super) fn comment_header(comment: &CommentRecord, now: i64, mode: ExplorerIconMode) -> Line {
    let mut rich = Rich::new()
        .push(&format!("{} ", glyph(Glyph::Person, mode)), role(GitRole::Muted))
        .push(&comment.author, strong(GitRole::Author));
    if comment.pending {
        rich = rich.sep().push("commented", role(GitRole::Muted)).sep().push("sending…", role(GitRole::Modified));
    } else {
        let when = records::ago(&comment.created, now);
        if !when.is_empty() {
            rich = rich.sep().push(&when, role(GitRole::Muted));
        }
    }
    rich.line()
}

fn loading_or_error(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            rows.push(Rich::new().push("✕ ", role(GitRole::Removed)).push(error, role(GitRole::Removed)).line());
            rows.push(muted(&format!("r refresh · signed out? run {}", sign_in(workspace.forge_kind()).0)));
            true
        }
        None => {
            rows.push(muted("Loading…"));
            true
        }
    }
}

/// Every comment and event of the issue, oldest first.
pub(super) fn entries(workspace: &Workspace, id: u64) -> Vec<TimelineEntry> {
    workspace
        .value(&Key::Timeline(id))
        .map(records::timeline)
        .unwrap_or_default()
}

pub(super) fn discussion_count(workspace: &Workspace, id: u64, record: &DetailRecord) -> u64 {
    let loaded = entries(workspace, id)
        .iter()
        .filter(|entry| matches!(entry, TimelineEntry::Comment(_)))
        .count() as u64;
    loaded.max(record.row.comments)
}

/// `None` until the timeline is read.
pub(super) fn activity_count(workspace: &Workspace, id: u64) -> Option<usize> {
    workspace.value(&Key::Timeline(id))?;
    Some(entries(workspace, id).iter().filter(|entry| !entry.conversation()).count())
}

fn comment_rows(rows: &mut Vec<Line>, comment: &CommentRecord, width: usize, now: i64, mode: ExplorerIconMode) {
    rows.push(comment_header(comment, now, mode));
    prose::markdown(rows, &comment.body, width, "");
}

/// The rows below the reader's pinned header.
pub(super) fn rows(workspace: &Workspace, width: usize, mode: ExplorerIconMode) -> Vec<Line> {
    let id = workspace.number();
    let mut rows = Vec::new();
    if id == 0 {
        return empty(workspace, width);
    }
    let key = Key::Item(1, id);
    let loaded = workspace.item();
    let Some(value) = loaded.or_else(|| workspace.list_item()) else {
        loading_or_error(workspace, &key, &mut rows);
        return rows;
    };
    let record = DetailRecord::from_value(value);
    let now = records::now();
    let column = 12;
    match workspace.issue_tab {
        1 => discussion(workspace, id, &mut rows, width, now, mode),
        2 => activity(workspace, id, &record, &mut rows, now, mode),
        _ => {
            if loaded.is_none() {
                if let Some(Err(error)) = workspace.cache.get(&key) {
                    unavailable(workspace, &mut rows, error, "r");
                } else {
                    rows.push(muted("Loading details…"));
                }
                rows.push(blank());
            } else {
                // A chip's padding space starts a column early so its text lines up.
                let labels = if record.row.labels.is_empty() {
                    field("Labels", column).push("None yet", role(GitRole::Muted))
                } else {
                    field("Labels", column - 1).labels(&record.row.labels)
                };
                let mut labels = labels.line();
                labels.action = Some(Action::Metadata(1, id, "labels"));
                rows.push(labels);
                let mut assignee = field("Assignee", column);
                assignee = if record.assignees.is_empty() {
                    assignee.push("No one", role(GitRole::Muted))
                } else {
                    let mut rich = assignee;
                    for (index, login) in record.assignees.iter().enumerate() {
                        if index > 0 {
                            rich = rich.push(", ", role(GitRole::Muted));
                        }
                        rich = rich.push(login, role(GitRole::Author));
                    }
                    rich
                };
                let mut assignee = assignee.line();
                assignee.action = Some(Action::Metadata(1, id, "assignees"));
                rows.push(assignee);
                if let Some(milestone) = &record.milestone {
                    let mut row = field("Milestone", column)
                        .push(&format!("{} ", glyph(Glyph::Milestone, mode)), role(GitRole::Tag))
                        .push(milestone, role(GitRole::Text))
                        .line();
                    row.action = Some(Action::Metadata(1, id, "milestone"));
                    rows.push(row);
                }
                if record.links.is_empty() && workspace.capabilities().linked_issues.allowed() {
                    let github = matches!(
                        workspace.forge_kind(),
                        None | Some(crate::forge::provider::ProviderKind::GitHub)
                    );
                    let mut row = field("Linked PR", column).push("None", role(GitRole::Muted));
                    if github {
                        // GitHub can make the branch for an issue; the checkout stays put.
                        row = row.plain("  ").push("› ", role(GitRole::Focus)).push("Create linked branch", role(GitRole::Link));
                    }
                    let mut row = row.line();
                    if github {
                        row.action = Some(Action::Develop(id));
                    }
                    rows.push(row);
                }
                for (index, link) in record.links.iter().enumerate() {
                    let lead = field(if index == 0 { "Linked PR" } else { "" }, column);
                    rows.push(link_row(workspace, link, true, lead, mode));
                }
                if !record.links.is_empty()
                    && matches!(workspace.forge_kind(), None | Some(crate::forge::provider::ProviderKind::GitHub))
                {
                    let mut row = field("", column)
                        .push("› ", role(GitRole::Focus))
                        .push("Create linked branch", role(GitRole::Link))
                        .line();
                    row.action = Some(Action::Develop(id));
                    rows.push(row);
                }
                rows.push(rule(width.min(prose::MEASURE)));
            }
            // A list row that omitted the body says nothing about it: no "No description".
            if loaded.is_some() || !value["body"].is_null() {
                prose::markdown(&mut rows, &record.body, width, "");
            }
            if loaded.is_none() {
                return rows;
            }
            // The newest comments, so the overview is never a dead end.
            let entries = entries(workspace, id);
            let comments = entries
                .iter()
                .filter_map(|entry| match entry {
                    TimelineEntry::Comment(comment) => Some(comment),
                    _ => None,
                })
                .collect::<Vec<_>>();
            rows.push(blank());
            if workspace.value(&Key::Timeline(id)).is_none() {
                loading_or_error(workspace, &Key::Timeline(id), &mut rows);
                return rows;
            }
            let total = discussion_count(workspace, id, &record);
            rows.push(heading(
                "Latest activity",
                &match total {
                    0 => "no comments yet".to_owned(),
                    1 => "1 comment".to_owned(),
                    n => format!("{n} comments"),
                },
            ));
            rows.push(blank());
            for comment in comments.iter().rev().take(2).rev() {
                comment_rows(&mut rows, comment, width, now, mode);
                rows.push(blank());
            }
            let mut more = Rich::new()
                .push("› ", role(GitRole::Focus))
                .push(if comments.len() > 2 { "Read the whole discussion" } else { "Open the discussion" }, role(GitRole::Link))
                .line();
            more.action = Some(Action::PrTab(1));
            rows.push(more);
            let mut reply = Rich::new()
                .push("› ", role(GitRole::Focus))
                .push("Write a comment", role(GitRole::Link))
                .plain("  ")
                .push("C", strong(GitRole::Focus))
                .line();
            reply.action = Some(Action::Comment(1, id));
            rows.push(reply);
            // The issue's own next step: close it, or reopen it.
            let open = record.row.lifecycle == crate::forge::model::Lifecycle::Open;
            let mut state = Rich::new()
                .push("› ", role(GitRole::Focus))
                .push(if open { "Close the issue" } else { "Reopen the issue" }, role(GitRole::Link))
                .plain("  ")
                .push(if open { "x" } else { "X" }, strong(GitRole::Focus))
                .line();
            state.action = Some(Action::State(1, id, open));
            rows.push(state);
        }
    }
    rows
}

fn discussion(workspace: &Workspace, id: u64, rows: &mut Vec<Line>, width: usize, now: i64, mode: ExplorerIconMode) {
    if loading_or_error(workspace, &Key::Timeline(id), rows) {
        return;
    }
    let entries = entries(workspace, id);
    let mut first = true;
    for entry in &entries {
        match entry {
            TimelineEntry::Comment(comment) => {
                if !first {
                    rows.push(rule(width.min(prose::MEASURE)));
                }
                first = false;
                comment_rows(rows, comment, width, now, mode);
            }
            TimelineEntry::Event { link: Some((link, pull)), .. } => {
                if !first {
                    rows.push(rule(width.min(prose::MEASURE)));
                }
                first = false;
                rows.push(link_row(workspace, link, *pull, Rich::new().push(if *pull { "PR " } else { "" }, role(GitRole::Muted)), mode));
            }
            _ => {}
        }
    }
    if first {
        rows.push(muted("No comments yet."));
    }
    rows.push(blank());
    let mut reply = Rich::new()
        .push("› ", role(GitRole::Focus))
        .push("Write a comment", role(GitRole::Link))
        .plain("  ")
        .push("C", strong(GitRole::Focus))
        .line();
    reply.action = Some(Action::Comment(1, id));
    rows.push(reply);
}

/// One event on the Activity rail: `● alex added [bug] · 1h ago`.
pub(super) fn event_row(workspace: &Workspace, entry: &TimelineEntry, now: i64, mode: ExplorerIconMode) -> Option<Line> {
    let TimelineEntry::Event {
        actor,
        verb,
        object,
        label,
        link,
        at,
        kind,
    } = entry
    else {
        return None;
    };
    let tone = match kind.as_str() {
        "closed" => GitRole::Removed,
        "reopened" => GitRole::Added,
        "merged" => GitRole::Ref,
        "labeled" | "unlabeled" => GitRole::Modified,
        "assigned" | "unassigned" | "review_requested" => GitRole::Author,
        _ => GitRole::Muted,
    };
    let mut rich = Rich::new()
        .push(&format!("{} ", glyph(Glyph::Dot, mode)), role(tone))
        .push(actor, strong(GitRole::Author))
        .plain(" ")
        .push(verb, role(GitRole::Muted));
    if let Some(label) = label {
        rich = rich.plain(" ").chip(&label.name, prose::label_style(label));
    } else if let Some(object) = object.as_ref().filter(|object| !object.is_empty()) {
        rich = rich.plain(" ").push(object, role(if kind == "merged" || kind.contains("commit") { GitRole::Hash } else { GitRole::Text }));
    }
    let mut action = None;
    if let Some((link, pull)) = link {
        let mark = glyph(if *pull { Glyph::PullRequest } else { Glyph::Issue }, mode);
        rich = rich
            .plain(" ")
            .push(&format!("{mark} {}{} {}", workspace.sigil(*pull), link.number, link.title), role(GitRole::Link));
        action = link_action(workspace, link, if *pull { 2 } else { 1 });
    }
    let when = records::ago(at, now);
    if !when.is_empty() {
        rich = rich.sep().push(&when, role(GitRole::Muted));
    }
    let mut row = rich.line();
    row.action = action;
    Some(row)
}

fn activity(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    now: i64,
    mode: ExplorerIconMode,
) {
    let opened = Rich::new()
        .push(&format!("{} ", glyph(Glyph::Dot, mode)), role(GitRole::Added))
        .push(&record.row.author, strong(GitRole::Author))
        .plain(" ")
        .push("opened this", role(GitRole::Muted));
    let when = records::ago(&record.created, now);
    rows.push(if when.is_empty() { opened.line() } else { opened.sep().push(&when, role(GitRole::Muted)).line() });
    if loading_or_error(workspace, &Key::Timeline(id), rows) {
        return;
    }
    let mut comments = 0;
    for entry in entries(workspace, id) {
        if let TimelineEntry::Comment(_) = entry {
            comments += 1;
            continue;
        }
        if let Some(row) = event_row(workspace, &entry, now, mode) {
            rows.push(Rich::new().push("│", role(GitRole::Border)).line());
            rows.push(row);
        }
    }
    if comments > 0 {
        rows.push(Rich::new().push("│", role(GitRole::Border)).line());
        let mut row = Rich::new()
            .push(&format!("{} ", glyph(Glyph::Comment, mode)), role(GitRole::Muted))
            .push(&format!("{comments} comment{} in Discussion", if comments == 1 { "" } else { "s" }), role(GitRole::Link))
            .line();
        row.action = Some(Action::PrTab(1));
        rows.push(row);
    }
}

/// X-EMPTY for a list with nothing in it: what the view is and what to do next.
pub(super) fn empty(workspace: &Workspace, width: usize) -> Vec<Line> {
    let mut rows = Vec::new();
    let what = if workspace.page == 1 { "issues" } else { workspace.terms().pr_plural };
    let what = what.to_lowercase();
    if workspace.inbox.enabled {
        rows.push(Rich::new().push("No indexed matches", strong(GitRole::Text)).line());
        rows.push(blank());
        rows.push(muted("/ filter · [ ] sections · r reload"));
        return rows;
    }
    rows.push(Rich::new().push(&format!("No {what} match this view"), strong(GitRole::Text)).line());
    rows.push(blank());
    for line in crate::ui::render::wrap_plain_text(
        "Your repository is connected. Change the filter with / or switch sections with [ and ].",
        width.clamp(10, prose::MEASURE),
        crate::config::AmbiguousWidth::Narrow,
    ) {
        rows.push(muted(&line));
    }
    rows.push(blank());
    let mut create = Rich::new()
        .push("› ", role(GitRole::Focus))
        .push(
            if workspace.page == 1 { "New issue".to_owned() } else { format!("New {}", workspace.terms().pr_singular.to_lowercase()) }.as_str(),
            role(GitRole::Link),
        )
        .plain("  ")
        .push("n", strong(GitRole::Focus))
        .line();
    create.action = Some(if workspace.page == 1 { Action::CreateIssue } else { Action::CreatePullRequest });
    rows.push(create);
    rows
}
