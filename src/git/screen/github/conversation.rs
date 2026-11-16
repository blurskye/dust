//! The Conversation tab of pull requests and the issue reader, laid out like GitHub's pages
//! (DESIGN.md §3.26): one chronological timeline of cards on the left, the metadata sidebar on
//! the right (a compact block above the timeline when the reader is narrow).
//!
//! Cards: the description, `maya added 2 commits` with its commits, one-line events on a thin
//! rail, comments (badge, age, `edited`, markdown, reactions) and reviews holding their inline
//! threads (file:line, the last lines of the hunk, the replies). Bot `<details>` sections,
//! resolved threads and long comments start folded; Enter on the `▸` row opens them.
use super::super::shell::glyphs::{Glyph, glyph};
use super::lines::{Line, SourceScope};
use super::prose::{self, Rich, blank, muted, role, strong};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::{Lifecycle, ReviewState};
use crate::forge::records::{self, CommentRecord, DetailRecord, ThreadCard, TimelineEntry};
use crate::ui::{Emphasis, GitChip, GitRole, GitSurface, Style};
use unicode_width::UnicodeWidthStr;

/// The reader width from which the metadata sidebar sits beside the timeline.
pub(super) const SIDEBAR_FROM: usize = 100;
/// A comment longer than this many rows shows its first [`LONG_KEEP`] and a `▸ N more lines`.
const LONG: usize = 20;
const LONG_KEEP: usize = 12;
/// GitHub's fold for a long timeline: past this many items the middle folds to
/// `N hidden items · Load more…` (Enter opens it), keeping the first and last [`FOLD_KEEP`].
const FOLD_OVER: usize = 60;
const FOLD_KEEP: usize = 20;
/// Diff lines a review thread shows above its comments.
const HUNK_LINES: usize = 4;

/// The sidebar's share of a wide reader: about 28%, never under 34 columns, so a wider pane
/// gives its values more room, not less (grade14 GH-github-05).
pub(super) fn sidebar_width(width: usize) -> usize {
    (width * 28 / 100).clamp(34, 48)
}

/// The timeline's width: beside the sidebar, or the whole reader.
pub(super) fn timeline_width(width: usize) -> usize {
    if width >= SIDEBAR_FROM { width - sidebar_width(width) - 3 } else { width }
}

/// A fold key inside this item's Conversation.
fn key(kind: &str, id: &str) -> String {
    format!("\u{0}conv\u{0}{kind}\u{0}{id}")
}

/// A review thread's fold key: toggled in `Workspace::collapsed`, it opens a resolved thread
/// or folds an open one.
#[cfg(test)]
pub(super) fn thread_key(id: &str) -> String {
    key("thread", id)
}

/// A card or section that starts folded opens when its key is in `collapsed`; one that starts
/// open folds when it is.
fn folded(workspace: &Workspace, number: u64, key: &str, by_default: bool) -> bool {
    by_default != workspace.collapsed.contains(&(number, key.to_owned()))
}

/// Where a long comment's fold may cut: never inside a boxed table, which reads as a
/// header-only box. A cut inside one moves up to the table's top, or past its bottom when
/// the table starts the comment.
/// Never more than `rows.len()`: every index is checked, since a panic here takes the reader
/// down.
fn whole_tables(rows: &[Line], keep: usize) -> usize {
    let keep = keep.min(rows.len());
    let edge = |row: &Line, corner: char| {
        row.text.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '▍' | '▎')).starts_with(corner)
    };
    let kept = rows.get(..keep).unwrap_or_default();
    let Some(open) = kept.iter().rposition(|row| edge(row, '┌')) else {
        return keep;
    };
    if kept.iter().skip(open).any(|row| edge(row, '└')) {
        return keep;
    }
    if open > 0 {
        return open;
    }
    // The table starts the comment: keep it whole, unless it runs to the end (or never
    // closes), where folding would hide nothing worth a row.
    rows.iter()
        .skip(keep)
        .position(|row| edge(row, '└'))
        .map_or(keep, |end| (keep + end + 1).min(rows.len().saturating_sub(1)).max(keep))
}

/// The middle a long timeline folds: none at [`FOLD_OVER`] items or fewer, so the subtraction
/// never runs short.
fn fold_range(total: usize) -> Option<std::ops::Range<usize>> {
    (total > FOLD_OVER && total >= 2 * FOLD_KEEP).then(|| FOLD_KEEP..total - FOLD_KEEP)
}

/// The timeline items a long conversation hides, while its fold is shut.
fn hidden_items(workspace: &Workspace, number: u64, total: usize) -> Option<std::ops::Range<usize>> {
    fold_range(total).filter(|_| folded(workspace, number, &key("hidden", "timeline"), true))
}

/// `┆ 140 hidden items · Load more…`, as GitHub folds a long conversation.
fn hidden_row(number: u64, hidden: usize) -> Line {
    let mut row = Rich::new()
        .push("  ┆ ", role(GitRole::Border))
        .push(&format!("{hidden} hidden items"), strong(GitRole::Text))
        .sep()
        .push("Load more…", role(GitRole::Link))
        .line();
    row.action = Some(Action::Collapse(number, key("hidden", "timeline")));
    row.card = true;
    row
}

fn badge() -> Style {
    Style::Git { role: GitRole::Muted, surface: GitSurface::Chip(GitChip::Muted), emphasis: Emphasis::None }
}

pub(super) fn is_bot(login: &str) -> bool {
    let login = login.to_ascii_lowercase();
    login.ends_with("[bot]") || login.ends_with("-bot") || login.contains("_bot")
}

/// GitHub's word for the author's relation to the repository.
fn association_word(association: &str) -> Option<&'static str> {
    match association {
        "OWNER" => Some("Owner"),
        "MEMBER" => Some("Member"),
        "COLLABORATOR" => Some("Collaborator"),
        "CONTRIBUTOR" => Some("Contributor"),
        "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER" => Some("First-time contributor"),
        _ => None,
    }
}

fn reaction_glyph(content: &str) -> &'static str {
    match content {
        "+1" | "thumbsup" => "👍",
        "-1" | "thumbsdown" => "👎",
        "laugh" => "😄",
        "hooray" | "tada" => "🎉",
        "confused" => "😕",
        "heart" => "💖",
        "rocket" => "🚀",
        "eyes" => "👀",
        _ => "·",
    }
}

/// `󰀄 alex  Member · commented 2h ago · edited` on the card's header band.
fn card_header(
    author: &str,
    badges: &[&str],
    action: &str,
    extra: Option<(&str, GitRole)>,
    when: &str,
    edited: bool,
    mode: ExplorerIconMode,
) -> Rich {
    let mut rich = Rich::new()
        .push(&format!(" {} ", glyph(Glyph::Person, mode)), role(GitRole::Muted))
        .push(author, strong(GitRole::Author));
    for word in badges {
        rich = rich.plain(" ").chip(word, badge());
    }
    if !action.is_empty() {
        rich = rich.plain(" ").push(action, role(GitRole::Muted));
    }
    if let Some((text, tone)) = extra {
        rich = rich.sep().push(text, strong(tone));
    }
    if !when.is_empty() {
        rich = rich.sep().push(when, role(GitRole::Muted));
    }
    if edited {
        rich = rich.sep().push("edited", role(GitRole::Muted));
    }
    rich
}

fn header_line(rich: Rich, fold: Option<(u64, String)>, web: &str) -> Line {
    let mut row = rich.line();
    row.band = Some(GitSurface::FileBand);
    row.card = true;
    if let Some((number, key)) = fold {
        row.action = Some(Action::Collapse(number, key));
    }
    if !web.is_empty() {
        row.web = Some(web.to_owned());
    }
    row
}

/// The thin rail between cards.
fn rail() -> Line {
    Rich::new().push("  │", role(GitRole::Border)).line()
}

/// Markdown with GitHub's `<details>` sections folded (Enter on `▸ summary` opens one) and a
/// long body cut to its first rows and `▸ N more lines`.
#[allow(clippy::too_many_arguments)]
fn body(
    workspace: &Workspace,
    number: u64,
    owner: &str,
    text: &str,
    rows: &mut Vec<Line>,
    width: usize,
    indent: &str,
    web: &str,
) {
    let start = rows.len();
    // `prose::markdown` takes the whole width and keeps the indent out of it itself.
    for (index, part) in details(text).into_iter().enumerate() {
        match part {
            Part::Text(text) => {
                if !text.trim().is_empty() {
                    prose::markdown(rows, &text, width, indent);
                }
            }
            Part::Details(summary, content) => {
                let fold = key("details", &format!("{owner}\u{0}{index}"));
                let shut = folded(workspace, number, &fold, true);
                let mark = if shut { "▸" } else { "▾" };
                let mut row = Rich::new()
                    .plain(indent)
                    .push(&format!("{mark} "), role(GitRole::Muted))
                    // Muted and underlined like a link, as GitHub draws a summary.
                    .push(
                        if summary.is_empty() { "Details" } else { summary.as_str() },
                        Style::Git { role: GitRole::Muted, surface: GitSurface::Pane, emphasis: Emphasis::Underline },
                    )
                    .line();
                row.action = Some(Action::Collapse(number, fold));
                row.card = true;
                rows.push(row);
                if !shut {
                    prose::markdown(rows, &content, width, &format!("{indent}  "));
                }
            }
        }
    }
    if rows.len() == start {
        rows.push(Rich::new().plain(indent).push("No description provided.", role(GitRole::Muted)).line());
    }
    let shown = rows.len() - start;
    let long = key("long", owner);
    // A long comment folds to its first rows; the description is the page's subject and
    // reads whole, as on the forge.
    if shown > LONG && owner != "description" {
        if folded(workspace, number, &long, true) {
            let keep = whole_tables(&rows[start..], LONG_KEEP);
            let hidden = shown - keep;
            rows.truncate(start + keep);
            let mut more = Rich::new()
                .plain(indent)
                .push(&format!("▸ {hidden} more lines"), role(GitRole::Link))
                .line();
            more.action = Some(Action::Collapse(number, long));
            more.card = true;
            rows.push(more);
        } else {
            let mut less = Rich::new().plain(indent).push("▾ show less", role(GitRole::Muted)).line();
            less.action = Some(Action::Collapse(number, long));
            less.card = true;
            rows.push(less);
        }
    }
    for row in &mut rows[start..] {
        if row.web.is_none() && !web.is_empty() {
            row.web = Some(web.to_owned());
        }
    }
}

enum Part {
    Text(String),
    /// `(summary, content)`.
    Details(String, String),
}

/// The body split at top-level `<details>…</details>` sections.
fn details(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut rest = text;
    loop {
        let Some(open) = find_tag(rest, "<details") else {
            // Blank text after the last section adds nothing.
            if parts.is_empty() || !rest.trim().is_empty() {
                parts.push(Part::Text(rest.to_owned()));
            }
            break;
        };
        let Some(open_end) = rest[open..].find('>').map(|at| open + at + 1) else {
            parts.push(Part::Text(rest.to_owned()));
            break;
        };
        // The matching close: count nested sections.
        let mut depth = 1;
        let mut at = open_end;
        let mut close = None;
        while depth > 0 {
            let next_open = find_tag(&rest[at..], "<details").map(|n| at + n);
            let next_close = find_tag(&rest[at..], "</details>").map(|n| at + n);
            match (next_open, next_close) {
                (Some(o), Some(c)) if o < c => {
                    depth += 1;
                    at = o + 8;
                }
                (_, Some(c)) => {
                    depth -= 1;
                    at = c + "</details>".len();
                    if depth == 0 {
                        close = Some(c);
                    }
                }
                _ => break,
            }
        }
        let Some(close) = close else {
            parts.push(Part::Text(rest.to_owned()));
            break;
        };
        if !rest[..open].trim().is_empty() {
            parts.push(Part::Text(rest[..open].to_owned()));
        }
        let mut inside = &rest[open_end..close];
        let mut summary = String::new();
        if let Some(start) = find_tag(inside, "<summary")
            && let Some(start_end) = inside[start..].find('>').map(|n| start + n + 1)
            && let Some(end) = find_tag(&inside[start_end..], "</summary>").map(|n| start_end + n)
        {
            summary = strip_tags(&inside[start_end..end]).trim().to_owned();
            inside = &inside[end + "</summary>".len()..];
            // Text before the summary is rare; it stays with the content.
        }
        parts.push(Part::Details(summary, inside.trim_matches('\n').to_owned()));
        rest = &rest[close + "</details>".len()..];
    }
    parts
}

fn find_tag(text: &str, tag: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(tag)
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

fn reactions_row(reactions: &[(String, u64)], indent: &str) -> Option<Line> {
    if reactions.is_empty() {
        return None;
    }
    let mut rich = Rich::new().plain(indent);
    for (index, (content, count)) in reactions.iter().enumerate() {
        if index > 0 {
            rich = rich.plain("  ");
        }
        rich = rich.chip(&format!("{} {count}", reaction_glyph(content)), badge());
    }
    Some(rich.line())
}

/// Badges for a comment's author: Bot, Author (the item's author), then the association.
fn badges<'a>(author: &str, item_author: &str, association: &'a str) -> Vec<&'a str> {
    let mut words = Vec::new();
    if is_bot(author) {
        words.push("Bot");
        return words;
    }
    if !item_author.is_empty() && author == item_author {
        words.push("Author");
    }
    if let Some(word) = association_word(association) {
        words.push(word);
    }
    words
}

/// One comment card.
#[allow(clippy::too_many_arguments)]
fn comment_card(
    workspace: &Workspace,
    number: u64,
    item_author: &str,
    comment: &CommentRecord,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    mode: ExplorerIconMode,
) {
    let owner = if comment.id.is_empty() { format!("{}{}", comment.author, comment.created) } else { comment.id.clone() };
    let fold = key("card", &owner);
    let shut = folded(workspace, number, &fold, false);
    let when = if comment.pending { "sending…".to_owned() } else { fresh::relative(&comment.created, now) };
    let words = badges(&comment.author, item_author, &comment.association);
    let mut header = card_header(&comment.author, &words, "commented", None, &when, comment.edited, mode);
    if shut {
        header = header.sep().push("folded", role(GitRole::Muted));
    }
    rows.push(header_line(header, Some((number, fold)), &comment.url));
    if shut {
        return;
    }
    body(workspace, number, &owner, &comment.body, rows, width, "  ", &comment.url);
    if let Some(row) = reactions_row(&comment.reactions, "  ") {
        rows.push(row);
    }
}

/// The last `HUNK_LINES` lines of a thread's hunk, banded like the Files diff.
fn hunk_rows(hunk: &str, rows: &mut Vec<Line>, width: usize, indent: &str) {
    let lines = hunk.lines().filter(|line| !line.starts_with("@@")).collect::<Vec<_>>();
    let from = lines.len().saturating_sub(HUNK_LINES);
    for line in &lines[from..] {
        let added = line.starts_with('+');
        let removed = line.starts_with('-');
        let text = super::super::shell::chrome::ellipsize(&line.replace('\t', "    "), width.saturating_sub(indent.width()).max(8));
        let mut row = Rich::new()
            .plain(indent)
            .push(&text, role(if added { GitRole::Added } else if removed { GitRole::Removed } else { GitRole::Text }))
            .line();
        row.band = Some(if added {
            GitSurface::AddedBand
        } else if removed {
            GitSurface::RemovedBand
        } else {
            GitSurface::Code
        });
        rows.push(row);
    }
}

/// A review thread inside its review card (or on its own when no review holds it).
#[allow(clippy::too_many_arguments)]
fn thread_card(
    workspace: &Workspace,
    number: u64,
    item_author: &str,
    thread: &ThreadCard,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    _mode: ExplorerIconMode,
) {
    let resolved = thread.resolved == Some(true);
    let fold = key("thread", &thread.id);
    let shut = folded(workspace, number, &fold, resolved);
    let reply = Action::Reply {
        number,
        thread: thread.id.clone(),
        repo: workspace.repo.clone(),
        host: workspace.host.clone(),
    };
    let place = match thread.line {
        Some(line) => format!("{}:{line}", thread.path),
        None => thread.path.clone(),
    };
    let (state, tone) = match thread.resolved {
        Some(true) => ("● Resolved", GitRole::Added),
        Some(false) => ("◔ Unresolved", GitRole::Modified),
        None => ("Conversation", GitRole::Muted),
    };
    let mut header = Rich::new()
        .plain("  ")
        .push(&format!("{} ", if shut { "▸" } else { "▾" }), role(GitRole::Muted))
        .push(&place, role(GitRole::Link))
        .plain("  ")
        .push(state, role(tone));
    if thread.outdated {
        header = header.sep().push("outdated", role(GitRole::Muted));
    }
    let replies = thread.replies();
    if shut {
        let comments = replies + 1;
        header = header.sep().push(&format!("{comments} comment{}", if comments == 1 { "" } else { "s" }), role(GitRole::Muted));
    }
    let mut header = header.line();
    header.band = Some(GitSurface::Code);
    header.card = true;
    header.unresolved = thread.resolved == Some(false);
    header.action = Some(Action::Collapse(number, fold));
    header.reply = Some(reply.clone());
    header.thread_header = true;
    let web = thread.comments.first().map(|comment| comment.url.clone()).unwrap_or_default();
    if !web.is_empty() {
        header.web = Some(web);
    }
    rows.push(header);
    if shut {
        return;
    }
    let start = rows.len();
    hunk_rows(&thread.hunk, rows, width, "    ");
    for (index, comment) in thread.comments.iter().enumerate() {
        let indent = if index == 0 { "    " } else { "      " };
        let when = fresh::relative(&comment.created, now);
        let words = badges(&comment.author, item_author, &comment.association);
        let mut who = Rich::new().plain(indent);
        if index > 0 {
            who = who.push("↳ ", role(GitRole::Muted));
        }
        who = who.push(&comment.author, strong(GitRole::Author));
        for word in &words {
            who = who.plain(" ").chip(word, badge());
        }
        if !when.is_empty() {
            who = who.sep().push(&when, role(GitRole::Muted));
        }
        if comment.edited {
            who = who.sep().push("edited", role(GitRole::Muted));
        }
        rows.push(who.line());
        let owner = if comment.id.is_empty() { format!("{}{index}", thread.id) } else { comment.id.clone() };
        let body_start = rows.len();
        match super::suggestion::split(&comment.body) {
            // A suggested change reads as a diff against the lines the thread covers (§3.30a).
            Some(parts) => {
                let count = super::suggestion::span(thread.start_line, thread.line);
                let original = super::suggestion::original(&thread.hunk, count);
                for (part_index, part) in parts.into_iter().enumerate() {
                    match part {
                        super::suggestion::Part::Text(text) => {
                            body(workspace, number, &format!("{owner}/{part_index}"), &text, rows, width, indent, &comment.url);
                        }
                        super::suggestion::Part::Suggestion(lines) => {
                            super::suggestion::rows(rows, &original, &lines, width, indent);
                        }
                    }
                }
            }
            None => body(workspace, number, &owner, &comment.body, rows, width, indent, &comment.url),
        }
        // Each logical line of the comment keeps its place across refreshes and reflow.
        let mut logical = 0;
        for row in &mut rows[body_start..] {
            if row.source.as_ref().is_some_and(|source| source.offset == 0) {
                logical += 1;
            }
            row.source_scope = Some(SourceScope::Thread(thread.id.clone(), index, logical));
        }
        if let Some(row) = reactions_row(&comment.reactions, indent) {
            rows.push(row);
        }
    }
    if thread.total > thread.comments.len() as u64 {
        rows.push(muted(&format!("      {} more on the forge · o opens it", thread.total - thread.comments.len() as u64)));
    }
    let draft = workspace
        .drafts
        .get(&workspace.draft_key(&reply))
        .and_then(|fields| fields.first())
        .is_some_and(|text| !text.trim().is_empty());
    let mut answer = Rich::new()
        .plain("    ")
        .push("› ", role(GitRole::Focus))
        .push(if draft { "Continue draft" } else { "Reply" }, role(GitRole::Link))
        .line();
    answer.action = Some(reply.clone());
    answer.card = true;
    rows.push(answer);
    if let Some(resolved) = thread.resolved {
        let mut toggle = Rich::new()
            .plain("    ")
            .push("› ", role(GitRole::Focus))
            .push(if resolved { "Unresolve conversation" } else { "Resolve conversation" }, role(GitRole::Link))
            .line();
        toggle.action = Some(Action::Resolve(thread.id.clone(), !resolved));
        toggle.card = true;
        rows.push(toggle);
    }
    if workspace.offers_commit(number, &thread.id) {
        let mut commit = Rich::new()
            .plain("    ")
            .push("› ", role(GitRole::Focus))
            .push("Commit suggestion", role(GitRole::Link))
            .line();
        commit.action = Some(Action::CommitSuggestion(number, thread.id.clone()));
        commit.card = true;
        rows.push(commit);
    }
    for row in &mut rows[start..] {
        if row.reply.is_none() {
            row.reply = Some(reply.clone());
        }
    }
}

fn review_words(state: ReviewState) -> (&'static str, GitRole) {
    match state {
        ReviewState::Approved => ("approved these changes", GitRole::Added),
        ReviewState::ChangesRequested => ("requested changes", GitRole::Removed),
        ReviewState::Dismissed => ("left a dismissed review", GitRole::Muted),
        _ => ("reviewed", GitRole::Muted),
    }
}

/// A timeline item with its time, for the one chronological stream.
enum Item<'a> {
    Description,
    Commits(Vec<&'a records::CommitRow>),
    Entry(TimelineEntry),
    /// One-line events [`super::issue::joins`] made one row.
    Events(Vec<TimelineEntry>),
    Thread(&'a ThreadCard),
}

fn commit_actor<'a>(commits: &[&'a records::CommitRow], fallback: &'a str) -> &'a str {
    commits.first().map_or(fallback, |commit| if commit.author.is_empty() { fallback } else { commit.author.as_str() })
}

/// The pull request's timeline cards. The merge box follows (`pr.rs`).
#[allow(clippy::too_many_arguments)]
pub(super) fn pull(
    workspace: &Workspace,
    number: u64,
    record: &DetailRecord,
    entries: Vec<TimelineEntry>,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    mode: ExplorerIconMode,
) {
    let threads = workspace
        .value(&Key::Threads(number))
        .map(records::threads)
        .unwrap_or_default();
    sidebar(workspace, number, record, &entries, &threads, true, rows, width, mode);
    let width = timeline_width(width);
    // Every review holds the threads its author opened up to its submission; the rest stand
    // on their own at their first comment's time.
    let reviews = entries
        .iter()
        .filter_map(|entry| match entry {
            TimelineEntry::Review { review } => Some(review.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut held: Vec<Vec<&ThreadCard>> = vec![Vec::new(); reviews.len()];
    let mut loose = Vec::new();
    for thread in &threads {
        let first = thread.comments.first();
        let created = first.map_or("", |comment| comment.created.as_str());
        let author = thread.author();
        let owner = reviews
            .iter()
            .enumerate()
            .filter(|(_, review)| review.author == author && (created.is_empty() || review.submitted.as_str() >= created))
            .min_by(|(_, a), (_, b)| a.submitted.cmp(&b.submitted))
            .map(|(at, _)| at);
        match owner {
            Some(at) => held[at].push(thread),
            None => loose.push(thread),
        }
    }
    let mut items: Vec<(String, Item)> = vec![(record.created.clone(), Item::Description)];
    for entry in entries {
        // Commits come from the commit list (every forge has one), not `committed` events.
        if matches!(&entry, TimelineEntry::Event { kind, .. } if kind == "committed") {
            continue;
        }
        items.push((entry.at().to_owned(), Item::Entry(entry)));
    }
    for commit in &record.commits {
        items.push((commit.committed.clone(), Item::Commits(vec![commit])));
    }
    for thread in loose {
        let at = thread.comments.first().map(|comment| comment.created.clone()).unwrap_or_default();
        items.push((at, Item::Thread(thread)));
    }
    // A stable sort by time: undated items keep their place after the ones before them.
    let mut last = String::new();
    for (at, _) in items.iter_mut() {
        if at.is_empty() {
            at.clone_from(&last);
        } else {
            last.clone_from(at);
        }
    }
    // The description opens the page whatever its stamp says (a commit or label can predate
    // the pull request's creation); everything else by time.
    items.sort_by(|a, b| {
        (!matches!(a.1, Item::Description), &a.0).cmp(&(!matches!(b.1, Item::Description), &b.0))
    });
    // Neighbouring commits are one `added N commits` card, and a forge's own `pushed N
    // commits` event beside them is that push again (Gitea, Forgejo): the card says it once.
    // Neighbouring events of one actor and kind are one line (`added bug, priority: high`).
    let pushed = |entry: &TimelineEntry| {
        matches!(entry, TimelineEntry::Event { verb, .. } if verb.starts_with("pushed ") && verb.contains(" commit"))
    };
    let mut merged: Vec<(String, Item)> = Vec::new();
    for (at, item) in items {
        match item {
            Item::Commits(mut commits) => {
                if matches!(merged.last(), Some((_, Item::Events(events))) if events.len() == 1 && pushed(&events[0])) {
                    merged.pop();
                }
                if let Some((_, Item::Commits(previous))) = merged.last_mut() {
                    previous.append(&mut commits);
                    continue;
                }
                merged.push((at, Item::Commits(commits)));
            }
            Item::Entry(entry @ TimelineEntry::Event { .. }) => {
                if pushed(&entry) && matches!(merged.last(), Some((_, Item::Commits(_)))) {
                    continue;
                }
                if let Some((_, Item::Events(events))) = merged.last_mut()
                    && events.last().is_some_and(|last| super::issue::joins(last, &entry, now))
                {
                    events.push(entry);
                    continue;
                }
                merged.push((at, Item::Events(vec![entry])));
            }
            item => merged.push((at, item)),
        }
    }
    let head = record.commits.last().map(|commit| commit.sha.as_str()).unwrap_or("");
    let ci = record.check_counts().state();
    let mut review_index = 0;
    let hidden = hidden_items(workspace, number, merged.len());
    for (index, (_, item)) in merged.iter().enumerate() {
        if let Some(range) = &hidden
            && range.contains(&index)
        {
            if index == range.start {
                rows.push(rail());
                rows.push(hidden_row(number, range.len()));
            }
            continue;
        }
        // One-line events stack without a connector row between them.
        let stacked = index > 0
            && hidden.as_ref().is_none_or(|range| index != range.end)
            && matches!(item, Item::Events(_)) && matches!(merged[index - 1].1, Item::Events(_));
        if index > 0 && !stacked {
            rows.push(rail());
        }
        match item {
            Item::Description => {
                let fold = key("card", "description");
                let shut = folded(workspace, number, &fold, false);
                let when = fresh::relative(&record.created, now);
                let words = badges(&record.row.author, "", "");
                let mut header = card_header(&record.row.author, &words, "commented", None, &when, false, mode);
                if shut {
                    header = header.sep().push("folded", role(GitRole::Muted));
                }
                rows.push(header_line(header, Some((number, fold)), &record.row.url));
                if !shut {
                    body(workspace, number, "description", &record.body, rows, width, "  ", &record.row.url);
                    // The description's reactions, as each comment's (GitHub, Gitea and GitLab).
                    if let Some(row) = reactions_row(&record.reactions, "  ") {
                        rows.push(row);
                    }
                }
            }
            Item::Commits(commits) => {
                let actor = commit_actor(commits, &record.row.author);
                let when = commits.last().map(|commit| fresh::relative(&commit.committed, now)).unwrap_or_default();
                let count = commits.len();
                let mut rich = Rich::new()
                    .push("  ●", role(GitRole::Ref))
                    .plain(" ")
                    .push(actor, strong(GitRole::Author))
                    .plain(" ")
                    .push(&format!("added {count} commit{}", if count == 1 { "" } else { "s" }), role(GitRole::Muted));
                if !when.is_empty() {
                    rich = rich.sep().push(&when, role(GitRole::Muted));
                }
                let mut row = rich.line();
                row.card = true;
                rows.push(row);
                // The subjects take what the row leaves after the rail (8 cells), one blank,
                // the check mark and the sha (9): cut only when that runs out, and the shas
                // sit one blank after the longest subject, never at a fixed far column
                // (R16 prs-09).
                let longest = commits.iter().map(|commit| commit.headline.width()).max().unwrap_or(0);
                let headline_width = longest.min(width.saturating_sub(18)).max(12);
                for commit in commits {
                    let short = commit.sha.chars().take(7).collect::<String>();
                    let mut rich = Rich::new()
                        .push("  │   ", role(GitRole::Border))
                        .push(&format!("{} ", glyph(Glyph::ForgeCommit, mode)), role(GitRole::Ref))
                        .push(&super::super::shell::chrome::ellipsize(&commit.headline, headline_width), role(GitRole::Text))
                        // One blank always before the status column, so a cut title never
                        // runs into it and every sha lines up.
                        .to_column(headline_width + 9);
                    if commit.sha == head
                        && let Some(state) = ci
                    {
                        let (mark, _, tone) = super::pr::ci_mark(state, mode);
                        rich = rich.push(&format!("{mark} "), role(tone));
                    } else {
                        rich = rich.plain("  ");
                    }
                    rich = rich.push(&short, role(GitRole::Hash));
                    let mut row = rich.line();
                    row.action = Some(Action::Commit(commit.sha.clone()));
                    row.card = true;
                    rows.push(row);
                }
            }
            Item::Events(events) => {
                let events = events.iter().collect::<Vec<_>>();
                if let Some(event) = super::issue::events_row(workspace, &events, now, width.saturating_sub(2), mode) {
                    for mut row in event {
                        indent(&mut row, "  ");
                        row.card = row.action.is_some();
                        rows.push(row);
                    }
                }
            }
            Item::Entry(entry @ TimelineEntry::Event { .. }) => {
                if let Some(event) = super::issue::event_row(workspace, entry, now, width.saturating_sub(2), mode) {
                    for mut row in event {
                        indent(&mut row, "  ");
                        row.card = row.action.is_some();
                        rows.push(row);
                    }
                }
            }
            Item::Entry(TimelineEntry::Comment(comment)) => {
                comment_card(workspace, number, &record.row.author, comment, rows, width, now, mode);
            }
            Item::Entry(TimelineEntry::Review { review }) => {
                let at = reviews
                    .iter()
                    .enumerate()
                    .skip(review_index)
                    .find(|(_, seen)| seen.id == review.id && seen.submitted == review.submitted)
                    .map(|(at, _)| at);
                if let Some(at) = at {
                    review_index = at + 1;
                }
                let owner = if review.id.is_empty() { format!("{}{}", review.author, review.submitted) } else { review.id.clone() };
                let fold = key("card", &owner);
                let shut = folded(workspace, number, &fold, false);
                let (verb, tone) = review_words(review.state);
                let (mark, _, _) = super::pr::review_mark(review.state, mode);
                let when = fresh::relative(&review.submitted, now);
                let words = badges(&review.author, &record.row.author, "");
                let extra = match review.state {
                    ReviewState::Approved | ReviewState::ChangesRequested => Some((format!("{mark} {verb}"), tone)),
                    _ => None,
                };
                let action = if extra.is_some() { "" } else { verb };
                let mut header = card_header(
                    &review.author,
                    &words,
                    action,
                    extra.as_ref().map(|(text, tone)| (text.as_str(), *tone)),
                    &when,
                    false,
                    mode,
                );
                let threads = at.map(|at| held[at].clone()).unwrap_or_default();
                if shut {
                    let open = threads.iter().filter(|thread| thread.resolved == Some(false)).count();
                    header = header.sep().push(
                        &format!("folded · {} thread{}{}", threads.len(), if threads.len() == 1 { "" } else { "s" }, if open > 0 { format!(", {open} open") } else { String::new() }),
                        role(GitRole::Muted),
                    );
                }
                rows.push(header_line(header, Some((number, fold)), ""));
                if shut {
                    continue;
                }
                if !review.body.trim().is_empty() {
                    body(workspace, number, &owner, &review.body, rows, width, "  ", "");
                }
                for thread in threads {
                    thread_card(workspace, number, &record.row.author, thread, rows, width, now, mode);
                }
            }
            Item::Thread(thread) => {
                thread_card(workspace, number, &record.row.author, thread, rows, width, now, mode);
            }
        }
    }
    if workspace.value(&Key::Timeline(number)).is_none() {
        rows.push(blank());
        rows.push(muted("Loading the conversation…"));
    }
    rows.push(rail());
    // The comment box, as on GitHub and as the issue page ends (`C` from anywhere).
    let mut comment = Rich::new()
        .push(&format!(" {} ", glyph(Glyph::Comment, mode)), role(GitRole::Muted))
        .push("Add a comment", strong(GitRole::Text))
        .line();
    comment.band = Some(GitSurface::FileBand);
    comment.card = true;
    comment.action = Some(Action::Comment(2, number));
    rows.push(comment);
    rows.push(rail());
}

/// The issue's Conversation: the sidebar, the timeline and the close/reopen box.
#[allow(clippy::too_many_arguments)]
pub(super) fn issue(
    workspace: &Workspace,
    number: u64,
    record: &DetailRecord,
    loaded: bool,
    show_body: bool,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    mode: ExplorerIconMode,
) {
    let entries = super::issue::entries(workspace, number);
    // The list's row lacks assignees, milestone and links: no sidebar claims "No one" until
    // the details are here.
    if loaded {
        sidebar(workspace, number, record, &entries, &[], false, rows, width, mode);
    }
    let width = timeline_width(width);
    let fold = key("card", "description");
    let shut = folded(workspace, number, &fold, false);
    let when = fresh::relative(&record.created, now);
    let words = badges(&record.row.author, "", "");
    let mut header = card_header(&record.row.author, &words, "commented", None, &when, false, mode);
    if shut {
        header = header.sep().push("folded", role(GitRole::Muted));
    }
    rows.push(header_line(header, Some((number, fold)), &record.row.url));
    if !shut && show_body {
        body(workspace, number, "description", &record.body, rows, width, "  ", &record.row.url);
        // The description's reactions, as each comment's (GitHub, Gitea and GitLab).
        if let Some(row) = reactions_row(&record.reactions, "  ") {
            rows.push(row);
        }
    }
    if !loaded {
        if !matches!(workspace.cache.get(&Key::Item(1, number)), Some(Err(_))) {
            rows.push(blank());
            rows.push(muted("Loading details…"));
        }
        return;
    }
    // Neighbouring events of one actor and kind are one line; one-line events stack without
    // a connector row between them.
    let mut groups: Vec<Vec<&TimelineEntry>> = Vec::new();
    for entry in &entries {
        if let Some(group) = groups.last_mut()
            && group.last().is_some_and(|last| super::issue::joins(last, entry, now))
        {
            group.push(entry);
        } else {
            groups.push(vec![entry]);
        }
    }
    let mut previous_event = false;
    let hidden = hidden_items(workspace, number, groups.len());
    for (index, group) in groups.into_iter().enumerate() {
        if let Some(range) = &hidden
            && range.contains(&index)
        {
            if index == range.start {
                rows.push(rail());
                rows.push(hidden_row(number, range.len()));
                previous_event = false;
            }
            continue;
        }
        let event = matches!(group[0], TimelineEntry::Event { .. });
        if !(event && previous_event) {
            rows.push(rail());
        }
        previous_event = event;
        match group[0] {
            TimelineEntry::Comment(comment) => {
                comment_card(workspace, number, &record.row.author, comment, rows, width, now, mode);
            }
            TimelineEntry::Event { .. } => {
                if let Some(event) = super::issue::events_row(workspace, &group, now, width.saturating_sub(2), mode) {
                    for mut row in event {
                        indent(&mut row, "  ");
                        row.card = row.action.is_some();
                        rows.push(row);
                    }
                }
            }
            TimelineEntry::Review { .. } => {}
        }
    }
    if workspace.value(&Key::Timeline(number)).is_none() {
        rows.push(blank());
        rows.push(muted("Loading the conversation…"));
    }
    rows.push(rail());
    // GitLab's `Close issue` is a plain button; GitHub's carries the closed-issue mark, which
    // on GitLab reads as a passed check (R16 gitlab-11).
    let marked = workspace.forge_kind() != Some(crate::forge::provider::ProviderKind::GitLab);
    composer(number, record.row.lifecycle == Lifecycle::Open, marked, rows, width, mode);
}

/// GitHub's comment box ending an issue (r15 ISS-05): a framed `Add a comment` box with its
/// primary `Comment` button, then the bordered secondary `Close issue` (or `Reopen issue`)
/// below its right edge. Each is one cursor stop.
fn composer(number: u64, open: bool, marked: bool, rows: &mut Vec<Line>, width: usize, mode: ExplorerIconMode) {
    let inner = width.saturating_sub(4).clamp(28, 96);
    let edge = role(GitRole::Border);
    let title = " Add a comment ";
    let top = Rich::new()
        .push("  ╭─", edge)
        .push(title, strong(GitRole::Text))
        .push(&format!("{}╮", "─".repeat(inner.saturating_sub(title.width() + 1))), edge)
        .line();
    rows.push(top);
    let button = " Comment ";
    let hint = "Leave a comment · C";
    let gap = inner.saturating_sub(hint.width() + button.width() + 2);
    let mut middle = Rich::new()
        .push("  │ ", edge)
        .push(hint, role(GitRole::Muted))
        .plain(&" ".repeat(gap))
        .push(button, Style::Git { role: GitRole::OnFocus, surface: GitSurface::ChipActive, emphasis: Emphasis::Bold })
        .push(" │", edge)
        .line();
    // One stop for the box, on its text row: the reader moves the cursor to the first row
    // with an action, so a second row with the same one would hold Tab in place.
    middle.action = Some(Action::Comment(1, number));
    middle.card = true;
    rows.push(middle);
    rows.push(Rich::new().push(&format!("  ╰{}╯", "─".repeat(inner)), edge).line());
    let (mark, words, tone) = if open {
        (glyph(Glyph::IssueClosed, mode), "Close issue", GitRole::Ref)
    } else {
        (glyph(Glyph::Issue, mode), "Reopen issue", GitRole::Added)
    };
    let mark = if marked { format!("{mark} ") } else { String::new() };
    let label = format!("{mark}{words}");
    let lead = (inner + 2).saturating_sub(label.width() + 4) + 2;
    let mut close = Rich::new()
        .plain(&" ".repeat(lead))
        .push("[ ", edge)
        .push(&mark, role(tone))
        .push(words, role(GitRole::Text))
        .push(" ]", edge)
        .line();
    close.action = Some(Action::State(1, number, open));
    close.card = true;
    rows.push(close);
}

/// GitHub's metadata sidebar. Beside the timeline its rows are `side`; on a narrow reader the
/// same facts are a compact block above it, ending in a rule.
#[allow(clippy::too_many_arguments)]
fn sidebar(
    workspace: &Workspace,
    number: u64,
    record: &DetailRecord,
    entries: &[TimelineEntry],
    threads: &[ThreadCard],
    pull: bool,
    rows: &mut Vec<Line>,
    width: usize,
    mode: ExplorerIconMode,
) {
    let page = if pull { 2 } else { 1 };
    let beside = width >= SIDEBAR_FROM;
    let side_width = if beside { sidebar_width(width) } else { width };
    let start = rows.len();
    let column = 14;
    // (heading, values, action): each value is one row beside the timeline, or joined on one
    // line in the compact block.
    let mut sections: Vec<(&str, Vec<Rich>, Option<Action>)> = Vec::new();
    if pull {
        let reviewers = record.reviewers();
        let values = if reviewers.is_empty() {
            vec![Rich::new().push("No reviews", role(GitRole::Muted))]
        } else {
            reviewers
                .iter()
                .map(|(login, state)| {
                    let (mark, _, tone) = super::pr::review_mark(*state, mode);
                    Rich::new().push(login, role(GitRole::Author)).plain(" ").push(mark, role(tone))
                })
                .collect()
        };
        if !(workspace.read_only() && reviewers.is_empty()) {
            sections.push(("Reviewers", values, None));
        }
    }
    let assignees = if record.assignees.is_empty() {
        // GitHub's own copy for an empty field (R16 issues-10).
        vec![Rich::new().push("No one assigned", role(GitRole::Muted))]
    } else {
        record.assignees.iter().map(|login| Rich::new().push(login, role(GitRole::Author))).collect()
    };
    sections.push(("Assignees", assignees, Some(Action::Metadata(page, number, "assignees"))));
    let labels = if record.row.labels.is_empty() {
        vec![Rich::new().push("None yet", role(GitRole::Muted))]
    } else {
        // Chips wrap to the sidebar's width.
        let mut lines = vec![Rich::new()];
        for label in &record.row.labels {
            let chip = label.name.width() + 2;
            if lines.last().is_some_and(|line| line.width() > 0 && line.width() + 1 + chip > side_width) {
                lines.push(Rich::new());
            }
            let line = lines.pop().unwrap_or_default();
            let line = if line.width() > 0 { line.plain(" ") } else { line };
            lines.push(line.chip(&label.name, prose::label_style(label)));
        }
        lines
    };
    sections.push(("Labels", labels, Some(Action::Metadata(page, number, "labels"))));
    // Every item keeps the Milestone row, `No milestone` when it has none, so the sidebar's
    // rows never move from one item to the next (R16 issues-10).
    let milestone = match &record.milestone {
        Some(milestone) => {
            Rich::new().push(&format!("{} ", glyph(Glyph::Milestone, mode)), role(GitRole::Tag)).push(milestone, role(GitRole::Text))
        }
        None => Rich::new().push("No milestone", role(GitRole::Muted)),
    };
    sections.push(("Milestone", vec![milestone], (!pull).then_some(Action::Metadata(1, number, "milestone"))));
    // Development: what this closes (pull requests) or what closes it (issues).
    let mut development = Vec::new();
    for link in &record.links {
        let mark = glyph(if pull { Glyph::Issue } else { Glyph::PullRequest }, mode);
        let text = format!("{mark} {}{} {}", workspace.sigil(!pull), link.number, link.title);
        let action = super::issue::link_action(workspace, link, if pull { 1 } else { 2 });
        if beside {
            // Beside the timeline a long title wraps under itself rather than ending in `…`.
            let lines = crate::ui::render::wrap_plain_text(&text, side_width.saturating_sub(2).max(8), crate::config::AmbiguousWidth::Narrow);
            for (index, line) in lines.iter().enumerate() {
                let line = if index == 0 { line.clone() } else { format!("  {line}") };
                development.push((Rich::new().push(&line, role(GitRole::Link)), action.clone()));
            }
        } else {
            development.push((Rich::new().push(&text, role(GitRole::Link)), action));
        }
    }
    if development.is_empty() {
        let none = if pull { "No linked issues" } else { "No branches or pull requests" };
        development.push((Rich::new().push(none, role(GitRole::Muted)), None));
    }
    if !pull && matches!(workspace.forge_kind(), None | Some(crate::forge::provider::ProviderKind::GitHub)) {
        development.push((
            Rich::new().push("› ", role(GitRole::Focus)).push("Create a branch", role(GitRole::Link)),
            Some(Action::Develop(number)),
        ));
    }
    // Participants: everyone who wrote on the page.
    let mut people: Vec<String> = vec![record.row.author.clone()];
    for entry in entries {
        let author = match entry {
            TimelineEntry::Comment(comment) => comment.author.as_str(),
            TimelineEntry::Review { review } => review.author.as_str(),
            TimelineEntry::Event { .. } => "",
        };
        if !author.is_empty() && !people.iter().any(|known| known == author) {
            people.push(author.to_owned());
        }
    }
    for thread in threads {
        for comment in &thread.comments {
            if !comment.author.is_empty() && !people.iter().any(|known| *known == comment.author) {
                people.push(comment.author.clone());
            }
        }
    }
    people.retain(|login| !login.is_empty());
    let mut participants = vec![Rich::new()];
    for login in &people {
        if participants.last().is_some_and(|line| line.width() > 0 && line.width() + 2 + login.width() > side_width) {
            participants.push(Rich::new());
        }
        let line = participants.pop().unwrap_or_default();
        let line = if line.width() > 0 { line.plain(", ") } else { line };
        participants.push(line.push(login, role(GitRole::Author)));
    }
    if beside {
        for (index, (title, values, action)) in sections.into_iter().enumerate() {
            if index > 0 {
                rows.push(blank());
            }
            let mut head = Rich::new().push(title, strong(GitRole::Muted)).line();
            head.action = action;
            rows.push(head);
            for value in values {
                rows.push(value.line());
            }
        }
        rows.push(blank());
        rows.push(Rich::new().push("Development", strong(GitRole::Muted)).line());
        for (value, action) in development {
            let mut row = value.line();
            row.action = action;
            rows.push(row);
        }
        rows.push(blank());
        rows.push(
            Rich::new()
                .push("Participants", strong(GitRole::Muted))
                .plain("  ")
                .push(&people.len().to_string(), role(GitRole::Muted))
                .line(),
        );
        for value in participants {
            rows.push(value.line());
        }
        for row in &mut rows[start..] {
            row.side = true;
            row.pan = false;
        }
        return;
    }
    // The compact block (GitHub mobile): one row per fact.
    for (title, values, action) in sections {
        let mut rich = prose::field(title, column);
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                rich = rich.plain(" ");
            }
            rich = append(rich, value);
        }
        let mut row = rich.line();
        row.action = action;
        rows.push(row);
    }
    for (index, (value, action)) in development.into_iter().enumerate() {
        let rich = prose::field(if index == 0 { "Development" } else { "" }, column);
        let mut row = append(rich, value).line();
        row.action = action;
        rows.push(row);
    }
    let mut rich = prose::field("Participants", column).push(&format!("{}  ", people.len()), role(GitRole::Muted));
    for (index, value) in participants.into_iter().enumerate() {
        if index > 0 {
            rich = rich.plain(", ");
        }
        rich = append(rich, value);
    }
    rows.push(rich.line());
    rows.push(prose::rule(width));
}

/// Where `m` puts the cursor: the merge control when there is one, else the box's header.
/// Whether the row at `cursor` is in the merge card, which ends the Conversation.
pub(super) fn in_merge_card(rows: &[Line], cursor: usize) -> bool {
    rows.iter().take(cursor + 1).any(|row| row.merge_box)
}

pub(super) fn merge_stop(rows: &[Line]) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row.action, Some(Action::Merge(..))))
        .or_else(|| rows.iter().position(|row| row.merge_box))
}

/// Moves a row right by `by`, its styles with it.
pub(super) fn indent(row: &mut Line, by: &str) {
    if row.text.is_empty() {
        return;
    }
    let shift = by.len();
    row.text.insert_str(0, by);
    for (range, _) in &mut row.paint {
        *range = range.start + shift..range.end + shift;
    }
    for (range, _) in &mut row.spans {
        *range = range.start + shift..range.end + shift;
    }
    for (range, _) in &mut row.bands {
        *range = range.start + shift..range.end + shift;
    }
}

/// `left` then `right`, keeping both's styles.
fn append(left: Rich, right: Rich) -> Rich {
    let mut left = left;
    let line = right.line();
    let mut at = 0;
    let mut paint = line.paint.clone();
    paint.sort_by_key(|(range, _)| range.start);
    for (range, style) in paint {
        if range.start > at {
            left = left.plain(&line.text[at..range.start]);
        }
        left = left.push(&line.text[range.clone()], style);
        at = range.end;
    }
    if at < line.text.len() {
        left = left.plain(&line.text[at..]);
    }
    left
}

/// The Conversation tab's count: comments, reviews with a body, and review comments.
pub(super) fn count(workspace: &Workspace, number: u64, entries: &[TimelineEntry]) -> usize {
    let comments = entries.iter().filter(|entry| entry.conversation()).count();
    let threads = workspace
        .value(&Key::Threads(number))
        .map(records::threads)
        .unwrap_or_default()
        .iter()
        .map(|thread| thread.comments.len())
        .sum::<usize>();
    comments + threads
}

/// Whether the reader shows a Conversation (j/k step by card there).
pub(super) fn showing(workspace: &Workspace) -> bool {
    workspace.page == 1 || (workspace.page == 2 && workspace.pr_tab == super::pr::CONVERSATION)
}

impl Workspace {
    /// The Conversation's own keys (§3.26, §5.7): `j`/`k` card to card, `Ctrl-d`/`Ctrl-u`
    /// half a page, `g`/`G` the ends, `n`/`N` the unresolved threads, Enter folds, `C`
    /// answers the focused card, `o` opens it on the forge, `m` goes to the merge box.
    /// Space is the leader and means nothing else here.
    pub(super) fn conversation_key(&mut self, key: &KeyEvent) -> Option<Result<Option<Form>, String>> {
        if self.focus != 2 || !showing(self) || key.modifiers.alt {
            return None;
        }
        let rows = &self.detail_rows;
        if rows.is_empty() {
            return None;
        }
        let last = rows.len() - 1;
        let first = rows.iter().position(|row| !row.side).unwrap_or(0);
        let stop = |row: &Line| row.card && !row.side;
        let cursor = self.detail_cursor.min(last);
        if key.modifiers.control {
            let half = (self.detail_visible / 2).max(1);
            match key.code {
                KeyCode::Char('d') => {
                    self.detail_cursor = (cursor + half).min(last).max(first);
                    self.detail_scroll = self.detail_scroll.saturating_add(half);
                }
                KeyCode::Char('u') => {
                    self.detail_cursor = cursor.saturating_sub(half).max(first);
                    self.detail_scroll = self.detail_scroll.saturating_sub(half);
                }
                _ => return None,
            }
            self.reading = false;
            return Some(Ok(None));
        }
        let pull = self.page == 2;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.detail_cursor = rows
                    .iter()
                    .enumerate()
                    .skip(cursor + 1)
                    .find(|(_, row)| stop(row))
                    .map_or(cursor.max(first), |(at, _)| at);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.detail_cursor = rows
                    .iter()
                    .enumerate()
                    .take(cursor)
                    .rev()
                    .find(|(_, row)| stop(row))
                    .map_or(first, |(at, _)| at);
            }
            KeyCode::Char('g') | KeyCode::Home => self.detail_cursor = first,
            KeyCode::Char('G') | KeyCode::End => self.detail_cursor = last,
            KeyCode::Char('n' | 'N') if pull => {
                let open = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(at, row)| row.unresolved.then_some(at))
                    .collect::<Vec<_>>();
                if open.is_empty() {
                    return Some(Err("No unresolved conversations".into()));
                }
                self.detail_cursor = if key.code == KeyCode::Char('n') {
                    *open.iter().find(|&&at| at > cursor).unwrap_or(&open[0])
                } else {
                    *open.iter().rev().find(|&&at| at < cursor).unwrap_or(&open[open.len() - 1])
                };
                self.reveal_top = true;
            }
            KeyCode::Char('m') if pull => {
                let Some(at) = merge_stop(rows) else {
                    return Some(Err("The merge box is loading".into()));
                };
                self.detail_cursor = at;
            }
            // The merge card's own keys (r15 PRS-10): on its Merge button or Method row Tab takes
            // the next method this forge allows; Enter on its text merges, as its footer says.
            KeyCode::Tab if pull && !key.modifiers.shift && matches!(rows[cursor].action, Some(Action::Merge(..) | Action::MergeMethod(_))) => {
                if self.merge_methods().len() < 2 {
                    return Some(Err("This forge offers one merge method".into()));
                }
                self.cycle_merge_method();
                return Some(Ok(None));
            }
            KeyCode::Enter if pull && in_merge_card(rows, cursor) && rows[cursor].action.is_none() => {
                let Some(at) = rows.iter().position(|row| matches!(row.action, Some(Action::Merge(..)))) else {
                    // pr.rs numbers reasons with "  1. ", then indents the merge card by two spaces.
                    if let Some(at) = rows.iter().enumerate()
                        .skip_while(|(_, row)| !row.merge_box)
                        .find(|(_, row)| row.text.starts_with("  1. ") || row.text.starts_with("    1. "))
                        .map(|(at, _)| at)
                    {
                        self.detail_cursor = at;
                    }
                    return Some(Ok(None));
                };
                let action = rows[at].action.clone()?;
                self.detail_cursor = at;
                return Some(self.activate(action));
            }
            KeyCode::Enter => {
                // Enter on a card's text folds the card it is in.
                let row = &rows[cursor];
                if row.action.is_some() {
                    return None;
                }
                let header = rows[..cursor]
                    .iter()
                    .rposition(|row| stop(row) && matches!(row.action, Some(Action::Collapse(..))))?;
                let action = rows[header].action.clone()?;
                self.detail_cursor = header;
                return Some(self.activate(action));
            }
            KeyCode::Char('C') => {
                let id = self.number();
                if id == 0 {
                    return None;
                }
                let action = rows[cursor].reply.clone().unwrap_or(Action::Comment(self.page, id));
                return Some(self.activate(action));
            }
            KeyCode::Char('o') => {
                let id = self.number();
                let url = rows[cursor].web.clone()?;
                return Some(self.activate(Action::Web(self.page, id, url)));
            }
            _ => return None,
        }
        self.reading = false;
        Some(Ok(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_rows(texts: &[&str]) -> Vec<Line> {
        texts.iter().map(|text| Rich::new().plain(text).line()).collect()
    }

    /// The fold arithmetic never indexes or subtracts past its input (forge_speed's
    /// `speed_audit` once panicked here on short timelines and comments).
    #[test]
    fn folds_never_index_past_their_input() {
        // Build 93 underflowed `total - FOLD_KEEP` on a short timeline.
        for total in [0, 1, FOLD_KEEP, 2 * FOLD_KEEP, FOLD_OVER] {
            assert_eq!(fold_range(total), None, "{total}");
        }
        assert_eq!(fold_range(FOLD_OVER + 1), Some(FOLD_KEEP..FOLD_OVER + 1 - FOLD_KEEP));
        for total in 0..200 {
            if let Some(range) = fold_range(total) {
                assert!(total > FOLD_OVER && range.start <= range.end && range.end <= total, "{total}");
            } else {
                assert!(total <= FOLD_OVER, "{total}");
            }
        }
        // A keep past the end, an empty comment, and a table that never closes.
        assert_eq!(whole_tables(&[], LONG_KEEP), 0);
        let short = plain_rows(&["one", "two"]);
        assert_eq!(whole_tables(&short, LONG_KEEP), 2);
        let open: Vec<&str> = std::iter::once("┌───┬───┐").chain(std::iter::repeat_n("│ a │ b │", 30)).collect();
        let open = plain_rows(&open);
        assert_eq!(whole_tables(&open, LONG_KEEP), LONG_KEEP);
        assert_eq!(whole_tables(&open, 100), open.len());
        // A table whose last row is short and has no bottom edge: cut nowhere inside it.
        let mut texts = vec!["Intro", ""];
        texts.push("┌───┬───┐");
        texts.extend(std::iter::repeat_n("│ a │ b │", 20));
        texts.push("│ a");
        let ragged = plain_rows(&texts);
        assert_eq!(whole_tables(&ragged, LONG_KEEP), 2);
        // A table that starts the comment and closes past the cut stays whole.
        let mut texts = vec!["┌───┬───┐"];
        texts.extend(std::iter::repeat_n("│ a │ b │", 14));
        texts.extend(["└───┴───┘", "after", "more"]);
        let first = plain_rows(&texts);
        assert_eq!(whole_tables(&first, LONG_KEEP), 16);
        // One closing on the comment's last row keeps the cut where it was.
        let mut texts = vec!["┌───┬───┐"];
        texts.extend(std::iter::repeat_n("│ a │ b │", 14));
        texts.push("└───┴───┘");
        let last = plain_rows(&texts);
        assert_eq!(whole_tables(&last, LONG_KEEP), last.len() - 1);
    }

    #[test]
    fn details_sections_split_out_with_their_summary() {
        let parts = details("Intro\n<details>\n<summary>Walkthrough</summary>\n\n| a | b |\n</details>\nAfter");
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], Part::Text(text) if text.trim() == "Intro"));
        assert!(matches!(&parts[1], Part::Details(summary, content) if summary == "Walkthrough" && content.contains("| a | b |")));
        assert!(matches!(&parts[2], Part::Text(text) if text.trim() == "After"));
        // Nested sections stay inside their parent.
        let nested = details("<details><summary>A</summary><details><summary>B</summary>x</details></details>");
        assert_eq!(nested.len(), 1);
        assert!(matches!(&nested[0], Part::Details(summary, content) if summary == "A" && content.contains("<summary>B")));
    }

    #[test]
    fn bots_and_badges() {
        assert!(is_bot("github-actions[bot]"));
        assert!(is_bot("coderabbit-bot"));
        assert!(is_bot("project_6329679_bot_7c2e1d"));
        assert!(!is_bot("robot"));
        assert_eq!(badges("maya", "maya", "MEMBER"), vec!["Author", "Member"]);
        assert_eq!(badges("coderabbitai[bot]", "maya", "NONE"), vec!["Bot"]);
        assert_eq!(badges("alex", "maya", "CONTRIBUTOR"), vec!["Contributor"]);
    }
}
