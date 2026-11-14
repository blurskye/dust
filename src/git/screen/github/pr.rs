//! F-PRINBOX overview, commits, checks, activity and the merge gate (DESIGN.md §3.26, §3.27,
//! §3.32) as reader rows built from typed records. The Files tab keeps the diff rows of
//! `lines::files` (§3.28), painted on the shared diff surfaces.
use super::super::shell::glyphs::{Glyph, glyph};
use super::issue::{self, comment_header, link_row};
use super::prose::{self, Rich, blank, field, heading, muted, role, rule, strong};
use super::lines::Line;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::{CiState, Lifecycle, ReviewState};
use crate::forge::records::{self, CheckCounts, DetailRecord, TimelineEntry};
use crate::ui::GitRole;

pub(super) const OVERVIEW: usize = 0;
pub(super) const COMMITS: usize = 1;
pub(super) const CHECKS: usize = 2;
pub(super) const FILES: usize = 3;
pub(super) const ACTIVITY: usize = 4;
pub(super) const MERGE: usize = 5;
/// Tabs `H`/`L` and `[`/`]` cycle through; the merge gate opens with `m`.
pub(super) const CYCLE: usize = 5;

pub(super) fn ci_mark(state: CiState, mode: ExplorerIconMode) -> (&'static str, &'static str, GitRole) {
    match state {
        CiState::Passed => (glyph(Glyph::CheckPassed, mode), "Passed", GitRole::Added),
        CiState::Failed | CiState::Cancelled => (glyph(Glyph::CheckFailed, mode), "Failed", GitRole::Removed),
        CiState::Pending | CiState::ActionRequired => (glyph(Glyph::CheckPending, mode), "Pending", GitRole::Modified),
        CiState::Skipped => (glyph(Glyph::CheckSkipped, mode), "Skipped", GitRole::Muted),
        CiState::Unknown => (glyph(Glyph::CheckUnknown, mode), "Unknown", GitRole::Muted),
    }
}

pub(super) fn review_mark(state: ReviewState, mode: ExplorerIconMode) -> (&'static str, &'static str, GitRole) {
    match state {
        ReviewState::Approved => (glyph(Glyph::CheckPassed, mode), "Approved", GitRole::Added),
        ReviewState::ChangesRequested => (glyph(Glyph::CheckFailed, mode), "Changes requested", GitRole::Removed),
        ReviewState::Requested => (glyph(Glyph::ReviewRequested, mode), "Review requested", GitRole::Muted),
        ReviewState::NotRequested => ("○", "Not requested", GitRole::Muted),
        ReviewState::Commented => (glyph(Glyph::Comment, mode), "Commented", GitRole::Muted),
        ReviewState::Dismissed => (glyph(Glyph::CheckSkipped, mode), "Dismissed", GitRole::Muted),
        ReviewState::Pending => (glyph(Glyph::CheckPending, mode), "Pending", GitRole::Modified),
    }
}

/// `All checks passed · 11 skipped`, `1 failed · 1 pending · 1 passed`, `No checks reported`;
/// `noun` is the forge's word (`checks`, GitLab's `jobs`). Skipped ones are never "passed".
pub(super) fn checks_summary(counts: &CheckCounts, noun: &str) -> String {
    if counts.total() == 0 {
        format!("No {noun} reported")
    } else if counts.failed == 0 && counts.pending == 0 && counts.unknown == 0 && counts.passed > 0 {
        if counts.skipped > 0 {
            format!("All {noun} passed · {} skipped", counts.skipped)
        } else {
            format!("All {noun} passed")
        }
    } else {
        counts.summary()
    }
}

fn threads_state(workspace: &Workspace, id: u64) -> Option<(usize, usize)> {
    workspace.value(&Key::Threads(id))?;
    let threads = workspace.review_threads(id);
    let unresolved = threads
        .iter()
        .filter(|thread| thread["isResolved"].as_bool() == Some(false))
        .count();
    Some((unresolved, threads.len()))
}

/// The pull request's timeline with the reviews the detail carries that the timeline lacks
/// (some forges keep review bodies only on the pull request), each placed by its time.
fn timeline(value: &Value, record: &DetailRecord) -> Vec<TimelineEntry> {
    let mut entries = records::timeline(value);
    for review in &record.reviews {
        if review.state == ReviewState::Commented && review.body.trim().is_empty() {
            continue;
        }
        let known = entries.iter().any(|entry| match entry {
            TimelineEntry::Review { review: seen } => {
                (!review.id.is_empty() && seen.id == review.id)
                    || (seen.author == review.author && seen.body == review.body)
            }
            _ => false,
        });
        if known {
            continue;
        }
        let at = if review.submitted.is_empty() {
            entries.len()
        } else {
            entries
                .iter()
                .position(|entry| !entry.at().is_empty() && entry.at() > review.submitted.as_str())
                .unwrap_or(entries.len())
        };
        entries.insert(at, TimelineEntry::Review { review: review.clone() });
    }
    entries
}

pub(super) fn counts(workspace: &Workspace, id: u64, record: &DetailRecord) -> [Option<usize>; 5] {
    let activity = workspace
        .value(&Key::Timeline(id))
        .map(|value| timeline(value, record).len() + workspace.review_threads(id).len());
    // Every tab counts once its data is here, 0 included (§3.27); before that, none.
    let loaded = workspace.value(&Key::Item(2, id)).is_some();
    [
        None,
        Some(record.commits.len()).filter(|_| loaded),
        Some(record.checks.len()).filter(|_| loaded),
        Some(record.files.len())
            .filter(|n| loaded || *n > 0)
            .or(record.row.changed_files.map(|n| n as usize)),
        activity,
    ]
}

pub(super) fn rows(workspace: &Workspace, width: usize, mode: ExplorerIconMode) -> Vec<Line> {
    let id = workspace.number();
    if id == 0 {
        return issue::empty(workspace, width);
    }
    let mut rows = Vec::new();
    let key = Key::Item(2, id);
    let loaded = workspace.item();
    let Some(value) = loaded.or_else(|| workspace.list_item()) else {
        loading(workspace, &key, &mut rows);
        return rows;
    };
    let record = DetailRecord::from_value(value);
    let now = records::now();
    if loaded.is_none() {
        if let Some(Err(error)) = workspace.cache.get(&key) {
            super::issue::unavailable(workspace, &mut rows, error, "R");
        } else {
            rows.push(muted("Loading details…"));
        }
        rows.push(blank());
        // A list row that omitted the body says nothing about it: no "No description".
        if workspace.pr_tab == OVERVIEW && !value["body"].is_null() {
            prose::markdown(&mut rows, &record.body, width, "");
        }
        return rows;
    }
    let item = loaded.expect("loaded");
    match workspace.pr_tab {
        COMMITS => commits(workspace, &record, &mut rows, width, now, mode),
        CHECKS => checks(&record, &mut rows, workspace.ci_words().2, mode),
        FILES => super::files::rows(workspace, id, item, &record, &mut rows, width, mode),
        ACTIVITY => activity(workspace, id, &record, &mut rows, width, now, mode),
        MERGE => merge(workspace, id, item, &record, &mut rows, mode),
        _ => overview(workspace, id, item, &record, &mut rows, width, now, mode),
    }
    rows
}

fn loading(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            rows.push(Rich::new().push("✕ ", role(GitRole::Removed)).push(error, role(GitRole::Removed)).line());
            rows.push(muted(&format!("R refresh · signed out? run {}", super::issue::sign_in(workspace.forge_kind()).0)));
            true
        }
        None => {
            rows.push(muted("Loading…"));
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn overview(
    workspace: &Workspace,
    id: u64,
    item: &Value,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    mode: ExplorerIconMode,
) {
    let column = 14;
    let label = |mark: Glyph, text: &str| field(&format!("{} {text}", glyph(mark, mode)), column);
    // Reviewers: each reviewer's latest word, then open requests.
    let reviewers = record.reviewers();
    // Anonymous reads carry no review decision or check names: those rows are absent, not
    // "No reviewers" (a signed-out reader cannot tell "none" from "not told").
    let blind = workspace.read_only();
    let mut row = label(Glyph::Person, "Reviewers");
    if reviewers.is_empty() {
        row = row.push("No reviewers", role(GitRole::Muted));
    }
    for (index, (login, state)) in reviewers.iter().enumerate() {
        if index > 0 {
            row = row.sep();
        }
        // `alex Requested` — the reviewer's own word, short.
        let (mark, word, tone) = review_mark(*state, mode);
        let word = match *state {
            ReviewState::Requested => workspace.requested_words().1,
            _ => word,
        };
        row = row
            .push(login, role(GitRole::Author))
            .plain(" ")
            .push(&format!("{mark} {word}"), role(tone));
    }
    if !(blind && reviewers.is_empty()) {
        rows.push(row.line());
    }
    let counts = record.check_counts();
    let (mark, _, tone) = ci_mark(counts.state().unwrap_or(CiState::Unknown), mode);
    let (_, ci_field, ci_noun) = workspace.ci_words();
    let mut row = field(&format!("{mark} {ci_field}"), column)
        .push(&checks_summary(&counts, ci_noun), role(if counts.total() == 0 { GitRole::Muted } else { tone }))
        .line();
    row.paint[0].1 = role(tone);
    row.action = Some(Action::PrTab(CHECKS));
    if !(blind && counts.total() == 0) {
        rows.push(row);
    }
    // A chip carries a space of padding; it starts a column early so its text lines up.
    let row = if record.row.labels.is_empty() {
        label(Glyph::Label, "Labels").push("None yet", role(GitRole::Muted))
    } else {
        field(&format!("{} Labels", glyph(Glyph::Label, mode)), column - 1)
            .labels(&prose::chips(&record.row.labels, record.row.lifecycle == Lifecycle::Draft))
    };
    let mut row = row.line();
    row.action = Some(Action::Metadata(2, id, "labels"));
    rows.push(row);
    // Files are listed once, under the description (each opens the Files tab on it); a
    // list without them yet keeps a summary row here instead.
    let files = record.files.len().max(record.row.changed_files.unwrap_or(0) as usize);
    let (additions, deletions) = (record.row.additions.unwrap_or(0), record.row.deletions.unwrap_or(0));
    if record.files.is_empty() && files > 0 {
        let mut row = label(Glyph::File, "Files")
            .push(&format!("{files} file{} changed", if files == 1 { "" } else { "s" }), role(GitRole::Text))
            .plain("  ")
            .push(&format!("+{additions}"), role(GitRole::Added))
            .plain(" ")
            .push(&format!("−{deletions}"), role(GitRole::Removed))
            .line();
        row.action = Some(Action::PrTab(FILES));
        rows.push(row);
    }
    if let Some(milestone) = &record.milestone {
        rows.push(label(Glyph::Milestone, "Milestone").push(milestone, role(GitRole::Text)).line());
    }
    for (index, link) in record.links.iter().enumerate() {
        let lead = if index == 0 {
            format!("{} Closes", glyph(Glyph::Link, mode))
        } else {
            String::new()
        };
        let lead = Rich::new().push(&lead, role(GitRole::Muted)).to_column(column);
        rows.push(link_row(workspace, link, false, lead, mode));
    }
    rows.push(rule(width.min(prose::MEASURE)));
    prose::markdown(rows, &record.body, width, "");
    if !record.files.is_empty() {
        rows.push(blank());
        let (added, removed) = record
            .files
            .iter()
            .fold((0, 0), |(a, d), file| (a + file.additions, d + file.deletions));
        rows.push(heading(
            "Files",
            &format!(
                "{} changed · +{added} −{removed}",
                record.files.len()
            ),
        ));
        let path_width = record
            .files
            .iter()
            .map(|file| unicode_width::UnicodeWidthStr::width(file.path.as_str()))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(18).max(12));
        for file in &record.files {
            let path = super::super::shell::chrome::ellipsize_start(&file.path, path_width);
            let tone = match file.status {
                'A' => GitRole::Added,
                'D' => GitRole::Removed,
                'R' => GitRole::Ref,
                _ => GitRole::Modified,
            };
            let mut row = Rich::new()
                .plain("  ")
                .push(&format!("{} ", glyph(Glyph::File, mode)), role(tone))
                .push(&path, role(GitRole::Link))
                .to_column(path_width + 6)
                .push(&format!("+{}", file.additions), role(GitRole::Added))
                .plain(" ")
                .push(&format!("−{}", file.deletions), role(GitRole::Removed))
                .line();
            row.action = Some(Action::SelectFile(id, file.path.clone()));
            rows.push(row);
        }
    }
    // The newest conversation, so the overview reads like the PR page.
    rows.push(blank());
    if let Some(value) = workspace.value(&Key::Timeline(id)) {
        let entries = timeline(value, record)
            .into_iter()
            .filter(TimelineEntry::conversation)
            .collect::<Vec<_>>();
        rows.push(heading(
            "Latest activity",
            &{
                let reviews = entries
                    .iter()
                    .filter(|entry| matches!(entry, TimelineEntry::Review { .. }))
                    .count();
                let comments = entries.len() - reviews;
                let count = |n: usize, word: &str| match n {
                    0 => String::new(),
                    1 => format!("1 {word}"),
                    n => format!("{n} {word}s"),
                };
                match (count(comments, "comment"), count(reviews, "review")) {
                    (c, r) if c.is_empty() && r.is_empty() => "no comments yet".to_owned(),
                    (c, r) if r.is_empty() => c,
                    (c, r) if c.is_empty() => r,
                    (c, r) => format!("{c} · {r}"),
                }
            },
        ));
        rows.push(blank());
        for entry in entries.iter().rev().take(2).rev() {
            conversation(rows, entry, width, now, mode);
            rows.push(blank());
        }
    } else {
        loading(workspace, &Key::Timeline(id), rows);
    }
    let action = |text: &str, key: &str, action: Action| {
        let mut row = Rich::new()
            .push("› ", role(GitRole::Focus))
            .push(text, role(GitRole::Link))
            .plain("  ")
            .push(key, strong(GitRole::Focus))
            .line();
        row.action = Some(action);
        row
    };
    next_step(workspace, id, item, record, rows, mode);
    rows.push(blank());
    rows.push(action("Review changes", "r", Action::Review(id)));
    rows.push(action("Merge readiness", "m", Action::PrTab(MERGE)));
    rows.push(action("Write a comment", "C", Action::Comment(2, id)));
}

/// §3.27: the overview ends on what the pull request is waiting for, so the pane answers
/// "what now" without a trip to the Merge tab.
fn next_step(
    workspace: &Workspace,
    id: u64,
    item: &Value,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    mode: ExplorerIconMode,
) {
    let (_, ci, noun) = workspace.ci_words();
    let counts = record.check_counts();
    let decision = item["reviewDecision"].as_str().unwrap_or("");
    let threads = threads_state(workspace, id);
    let pass = glyph(Glyph::CheckPassed, mode);
    let fail = glyph(Glyph::CheckFailed, mode);
    let wait = glyph(Glyph::CheckPending, mode);
    let (mark, tone, title, detail): (&str, GitRole, String, String) = match record.row.lifecycle {
        Lifecycle::Merged => (glyph(Glyph::PullMerged, mode), GitRole::Ref, "Merged".into(), "nothing left to do".into()),
        Lifecycle::Closed | Lifecycle::ClosedNotPlanned => (fail, GitRole::Removed, "Closed".into(), "X reopens it".into()),
        Lifecycle::Draft => (wait, GitRole::Muted, "Draft".into(), "mark it ready for review before merging".into()),
        Lifecycle::Open if record.mergeable == "CONFLICTING" => {
            (fail, GitRole::Removed, "Conflicts".into(), "resolve them before merging".into())
        }
        Lifecycle::Open if matches!(counts.state(), Some(CiState::Failed | CiState::Cancelled)) => (
            fail,
            GitRole::Removed,
            format!("{ci} failing"),
            format!("{} · the {ci} tab has the logs", counts.summary()),
        ),
        Lifecycle::Open if decision == "CHANGES_REQUESTED" => {
            (fail, GitRole::Modified, "Changes requested".into(), "push fixes, then ask for another review".into())
        }
        Lifecycle::Open if decision == "REVIEW_REQUIRED" => {
            (wait, GitRole::Modified, "Review required".into(), "r starts a review".into())
        }
        Lifecycle::Open if matches!(counts.state(), Some(CiState::Pending | CiState::ActionRequired)) => (
            wait,
            GitRole::Modified,
            format!("{ci} running"),
            counts.summary(),
        ),
        Lifecycle::Open if threads.is_some_and(|(open, _)| open > 0) => (
            "○",
            GitRole::Modified,
            "Unresolved conversations".into(),
            "resolve them in Activity".into(),
        ),
        Lifecycle::Open if record.merge_state == "CLEAN" => {
            (pass, GitRole::Added, "Ready to merge".into(), "m opens the merge gate".into())
        }
        Lifecycle::Open if counts.total() == 0 => {
            (wait, GitRole::Muted, format!("No {noun} reported"), "m shows what the merge needs".into())
        }
        Lifecycle::Open => (wait, GitRole::Muted, "Waiting on the forge".into(), "m shows what the merge needs".into()),
    };
    if rows.last().is_some_and(|row| !row.text.is_empty()) {
        rows.push(blank());
    }
    rows.push(heading("Next step", ""));
    let mut line = Rich::new()
        .plain("  ")
        .push(&format!("{mark} "), role(tone))
        .push(&title, strong(tone));
    if !detail.is_empty() {
        line = line.sep().push(&detail, role(GitRole::Muted));
    }
    rows.push(line.line());
}

fn conversation(rows: &mut Vec<Line>, entry: &TimelineEntry, width: usize, now: i64, mode: ExplorerIconMode) {
    match entry {
        TimelineEntry::Comment(comment) => {
            rows.push(comment_header(comment, now, mode));
            prose::markdown(rows, &comment.body, width, "");
        }
        TimelineEntry::Review { review } => {
            let (mark, word, tone) = review_mark(review.state, mode);
            let mut header = Rich::new()
                .push(&format!("{} ", glyph(Glyph::Person, mode)), role(GitRole::Muted))
                .push(&review.author, strong(GitRole::Author))
                .sep()
                .push(&format!("{mark} {word}"), role(tone));
            let when = records::ago(&review.submitted, now);
            if !when.is_empty() {
                header = header.sep().push(&when, role(GitRole::Muted));
            }
            rows.push(header.line());
            if !review.body.trim().is_empty() {
                prose::markdown(rows, &review.body, width, "");
            }
        }
        TimelineEntry::Event { .. } => {}
    }
}

fn commits(
    workspace: &Workspace,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    width: usize,
    now: i64,
    mode: ExplorerIconMode,
) {
    if record.commits.is_empty() {
        rows.push(muted("No commits reported."));
        return;
    }
    let headline_width = width.saturating_sub(34).max(16);
    for commit in &record.commits {
        let selected = workspace.selected_commit.as_deref() == Some(commit.sha.as_str());
        let short = commit.sha.chars().take(7).collect::<String>();
        let headline = super::super::shell::chrome::ellipsize(&commit.headline, headline_width);
        let mut row = Rich::new()
            .push(
                &format!("{} ", glyph(if selected { Glyph::CommitHead } else { Glyph::Commit }, mode)),
                role(if selected { GitRole::Focus } else { GitRole::Ref }),
            )
            .push(&short, role(GitRole::Hash))
            .plain(" ")
            .push(&headline, if selected { strong(GitRole::Text) } else { role(GitRole::Text) })
            .to_column(headline_width + 12);
        if !commit.author.is_empty() {
            row = row.push(&commit.author, role(GitRole::Author));
        }
        let when = records::ago(&commit.committed, now);
        if !when.is_empty() {
            row = row.sep().push(&when, role(GitRole::Muted));
        }
        let mut row = row.line();
        row.action = Some(Action::Commit(commit.sha.clone()));
        rows.push(row);
    }
    let Some(sha) = &workspace.selected_commit else {
        // No diff picked yet: the range itself, from the data already here, so the lower
        // half reads as a summary rather than a promise.
        commit_summary(record, rows, now, mode);
        return;
    };
    rows.push(blank());
    let key = Key::Commit(sha.clone());
    if loading(workspace, &key, rows) {
        return;
    }
    let preview = super::super::preview::Preview::git(workspace.raw(&key).unwrap_or_default(), None);
    for source in preview.rows {
        let mut row = Line::text(&source.text, source.style);
        if source.old.is_some() || source.new.is_some() {
            row.source = Some(find::Source::new(&source.text));
        }
        row.diff = match source.marker {
            Some('+') => Some(true),
            Some('-') => Some(false),
            _ => None,
        };
        row.spans = source.spans;
        if let Some(marker) = source.marker {
            row.gutter = format!(
                "{:>4} {:>4} {marker}",
                source.old.map(|n| n.to_string()).unwrap_or_default(),
                source.new.map(|n| n.to_string()).unwrap_or_default()
            );
        }
        rows.push(row);
    }
}

/// `3d 4h`, `5h 12m`, `40m`: how long a commit range took.
fn span_words(seconds: i64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds % 86_400 / 3600, seconds % 3600 / 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// What the commit range holds: branches, authors, span, size and the head merge pins.
fn commit_summary(record: &DetailRecord, rows: &mut Vec<Line>, now: i64, mode: ExplorerIconMode) {
    let column = 12;
    let mut authors: Vec<(&str, usize)> = Vec::new();
    for commit in &record.commits {
        if commit.author.is_empty() {
            continue;
        }
        match authors.iter_mut().find(|(name, _)| *name == commit.author) {
            Some((_, count)) => *count += 1,
            None => authors.push((&commit.author, 1)),
        }
    }
    rows.push(blank());
    rows.push(heading("Range", ""));
    if !record.base.is_empty() && !record.head.is_empty() {
        rows.push(
            field("Branches", column)
                .push(&record.base, role(GitRole::Ref))
                .push(" ← ", role(GitRole::Muted))
                .push(&record.head, role(GitRole::Ref))
                .line(),
        );
    }
    if !authors.is_empty() {
        let mut row = field("Authors", column);
        for (index, (name, count)) in authors.iter().enumerate() {
            if index > 0 {
                row = row.sep();
            }
            row = row.push(name, role(GitRole::Author)).push(&format!(" {count}"), role(GitRole::Muted));
        }
        rows.push(row.line());
    }
    let mut stamps = record.commits.iter().filter_map(|commit| records::epoch(&commit.committed).map(|at| (at, commit)));
    if let Some(first) = stamps.next() {
        let (oldest, newest) = stamps.fold((first, first), |(low, high), next| {
            (if next.0 < low.0 { next } else { low }, if next.0 > high.0 { next } else { high })
        });
        let span = newest.0 - oldest.0;
        let mut row = field("Span", column);
        row = if span < 60 {
            row.push(&format!("committed {}", records::ago(&newest.1.committed, now)), role(GitRole::Text))
        } else {
            row.push(&format!("over {}", span_words(span)), role(GitRole::Text))
                .sep()
                .push(&format!("newest {}", records::ago(&newest.1.committed, now)), role(GitRole::Muted))
        };
        rows.push(row.line());
    }
    if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions) {
        let files = if record.files.is_empty() { record.row.changed_files.unwrap_or(0) as usize } else { record.files.len() };
        let mut row = field("Changes", column)
            .push(&format!("+{add}"), role(GitRole::Added))
            .plain(" ")
            .push(&format!("−{del}"), role(GitRole::Removed));
        if files > 0 {
            row = row.push(&format!(" in {files} file{}", if files == 1 { "" } else { "s" }), role(GitRole::Muted));
        }
        rows.push(row.line());
    }
    if let Some(head) = record.commits.last() {
        let short = head.sha.chars().take(7).collect::<String>();
        rows.push(
            field("Head", column)
                .push(&format!("{} ", glyph(Glyph::CommitHead, mode)), role(GitRole::Ref))
                .push(&short, role(GitRole::Hash))
                .plain(" ")
                .push(&head.headline, role(GitRole::Text))
                .line(),
        );
    }
    rows.push(blank());
    rows.push(muted("Enter on a commit previews its diff here"));
}

fn checks(record: &DetailRecord, rows: &mut Vec<Line>, noun: &str, mode: ExplorerIconMode) {
    if record.checks.is_empty() {
        rows.push(muted(&format!("No {noun} reported for this head.")));
        return;
    }
    let counts = record.check_counts();
    let (mark, _, tone) = ci_mark(counts.state().unwrap_or(CiState::Unknown), mode);
    rows.push(
        Rich::new()
            .push(&format!("{mark} "), role(tone))
            .push(&checks_summary(&counts, noun), strong(tone))
            .line(),
    );
    rows.push(blank());
    let name_width = record
        .checks
        .iter()
        .map(|check| unicode_width::UnicodeWidthStr::width(check.name.as_str()))
        .max()
        .unwrap_or(0)
        .clamp(12, 40);
    // Grouped by workflow (the GitHub web layout): the name heads its checks once.
    let mut groups: Vec<(&str, Vec<&records::CheckRow>)> = Vec::new();
    for check in &record.checks {
        match groups.iter_mut().find(|(workflow, _)| *workflow == check.workflow.as_str()) {
            Some((_, checks)) => checks.push(check),
            None => groups.push((check.workflow.as_str(), vec![check])),
        }
    }
    let grouped = groups.len() > 1 || groups.first().is_some_and(|(workflow, _)| !workflow.is_empty());
    for (index, (workflow, checks)) in groups.iter().enumerate() {
        if grouped {
            if index > 0 {
                rows.push(blank());
            }
            let mut group = CheckCounts::default();
            for check in checks {
                group.add(check.state);
            }
            let name = if workflow.is_empty() { "Other" } else { workflow };
            rows.push(heading(name, &group.summary()));
        }
        for check in checks {
            let (mark, _, tone) = ci_mark(check.state, mode);
            let name = super::super::shell::chrome::ellipsize(&check.name, name_width);
            let mut row = Rich::new()
                .plain(if grouped { "  " } else { "" })
                .push(&format!("{mark} "), role(tone))
                .push(&name, role(GitRole::Text))
                .to_column(name_width + 4 + if grouped { 2 } else { 0 })
                .push(
                    match check.word.as_str() {
                        "success" => "passed",
                        "failure" => "failed",
                        "in progress" | "in_progress" | "queued" => "running",
                        word => word,
                    },
                    role(tone),
                )
                .to_column(name_width + 16 + if grouped { 2 } else { 0 })
                .push(&check.duration.map(records::duration).unwrap_or_default(), role(GitRole::Muted))
                .to_column(name_width + 26 + if grouped { 2 } else { 0 });
            if check.required {
                row = row.push("required", role(GitRole::Modified)).plain("  ");
            }
            let mut action = None;
            if let Some(run) = check.run {
                row = row.push(&format!("{} logs", glyph(Glyph::Link, mode)), role(GitRole::Link));
                action = Some(Action::Run(run));
            }
            let mut row = row.line();
            row.action = action;
            rows.push(row);
        }
    }
    check_digest(record, rows, noun, mode);
}

/// Below the list: what needs a look first, and where the time went.
fn check_digest(record: &DetailRecord, rows: &mut Vec<Line>, noun: &str, mode: ExplorerIconMode) {
    let column = 12;
    let failing = record
        .checks
        .iter()
        .filter(|check| matches!(check.state, CiState::Failed | CiState::Cancelled))
        .collect::<Vec<_>>();
    let running = record
        .checks
        .iter()
        .filter(|check| matches!(check.state, CiState::Pending | CiState::ActionRequired))
        .count();
    rows.push(blank());
    if !failing.is_empty() {
        rows.push(heading("Needs attention", &format!("{} failing", failing.len())));
        for check in &failing {
            let (mark, _, tone) = ci_mark(check.state, mode);
            let mut row = Rich::new()
                .plain("  ")
                .push(&format!("{mark} "), role(tone))
                .push(&check.name, strong(GitRole::Text));
            if !check.workflow.is_empty() {
                row = row.sep().push(&check.workflow, role(GitRole::Muted));
            }
            if let Some(seconds) = check.duration {
                row = row.sep().push(&format!("failed after {}", records::duration(seconds)), role(GitRole::Removed));
            }
            let mut row = row.line();
            row.action = check.run.map(Action::Run);
            rows.push(row);
        }
        rows.push(blank());
    }
    let timed = record.checks.iter().filter_map(|check| check.duration.map(|seconds| (seconds, check))).collect::<Vec<_>>();
    rows.push(heading("Timing", ""));
    if let Some((longest, slowest)) = timed.iter().max_by_key(|(seconds, _)| *seconds) {
        let total: u64 = timed.iter().map(|(seconds, _)| seconds).sum();
        rows.push(
            field("Slowest", column)
                .push(&slowest.name, role(GitRole::Text))
                .sep()
                .push(&records::duration(*longest), role(GitRole::Modified))
                .line(),
        );
        rows.push(
            field("Compute", column)
                .push(&records::duration(total), role(GitRole::Text))
                .push(&format!(" across {} {noun}", timed.len()), role(GitRole::Muted))
                .line(),
        );
    } else {
        rows.push(field("Durations", column).push("not reported by the forge", role(GitRole::Muted)).line());
    }
    if running > 0 {
        rows.push(
            field("Running", column)
                .push(&format!("{running} still going · R refreshes"), role(GitRole::Modified))
                .line(),
        );
    }
    if record.checks.iter().any(|check| check.run.is_some()) {
        rows.push(blank());
        rows.push(muted(&format!("Enter on a {} opens its log in the run view", noun.trim_end_matches('s'))));
    }
}

fn activity(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    width: usize,
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
    if !loading(workspace, &Key::Timeline(id), rows) {
        for entry in timeline(workspace.value(&Key::Timeline(id)).expect("loaded"), record) {
            rows.push(Rich::new().push("│", role(GitRole::Border)).line());
            if let Some(row) = issue::event_row(workspace, &entry, now, mode) {
                rows.push(row);
            } else {
                let start = rows.len();
                conversation(rows, &entry, width.saturating_sub(2), now, mode);
                for row in &mut rows[start + 1..] {
                    row.gutter = format!("│ {}", row.gutter);
                    row.gutter_style = role(GitRole::Border);
                }
            }
        }
    }
    rows.push(blank());
    let threads = threads_state(workspace, id);
    rows.push(heading(
        "Review threads",
        &match threads {
            Some((0, 0)) => "none".to_owned(),
            Some((0, total)) => format!("{total} resolved"),
            Some((open, total)) => format!("{open} of {total} unresolved"),
            None => String::new(),
        },
    ));
    rows.push(blank());
    super::files::thread_cards(workspace, id, rows, width, None);
}

fn merge(
    workspace: &Workspace,
    id: u64,
    item: &Value,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    mode: ExplorerIconMode,
) {
    let pass = glyph(Glyph::CheckPassed, mode);
    let fail = glyph(Glyph::CheckFailed, mode);
    let wait = glyph(Glyph::CheckPending, mode);
    let gate = |rows: &mut Vec<Line>, mark: &str, tone: GitRole, title: &str, detail: &str| {
        let mut rich = Rich::new()
            .push(&format!("{mark} "), role(tone))
            .push(title, strong(tone));
        if !detail.is_empty() {
            rich = rich.sep().push(detail, role(GitRole::Muted));
        }
        rows.push(rich.line());
    };
    rows.push(heading("Merge readiness", ""));
    rows.push(blank());
    // Each gate that holds the merge back, with what clears it, for the card below.
    let mut blockers: Vec<(String, String)> = Vec::new();
    let base = if record.base.is_empty() { "the base branch" } else { record.base.as_str() };
    match record.row.lifecycle {
        Lifecycle::Merged => gate(rows, glyph(Glyph::PullMerged, mode), GitRole::Ref, "Merged", "nothing left to do"),
        Lifecycle::Closed | Lifecycle::ClosedNotPlanned => {
            gate(rows, fail, GitRole::Removed, "Closed", "reopen with X before merging");
            blockers.push(("It is closed".into(), "X reopens it".into()));
        }
        Lifecycle::Draft => {
            gate(rows, wait, GitRole::Muted, "Draft", "mark it ready for review first");
            blockers.push(("It is a draft".into(), "mark it ready for review".into()));
        }
        Lifecycle::Open => {}
    }
    match record.mergeable.as_str() {
        "MERGEABLE" => gate(rows, pass, GitRole::Added, "No conflicts", &format!("can merge into {base}")),
        "CONFLICTING" => {
            gate(rows, fail, GitRole::Removed, "Conflicts", &format!("resolve conflicts with {base} first"));
            blockers.push((format!("Conflicts with {base}"), format!("merge or rebase {base} into the branch")));
        }
        _ => gate(rows, wait, GitRole::Muted, "Mergeability unknown", "the forge is still checking"),
    }
    let decision = item["reviewDecision"].as_str().unwrap_or("");
    let requested_changes = record
        .reviewers()
        .into_iter()
        .filter(|(_, state)| *state == ReviewState::ChangesRequested)
        .map(|(login, _)| login)
        .collect::<Vec<_>>();
    match decision {
        "APPROVED" => gate(rows, pass, GitRole::Added, "Approved", ""),
        "CHANGES_REQUESTED" => {
            let who = if requested_changes.is_empty() {
                String::new()
            } else {
                format!("{} requested changes", requested_changes.join(", "))
            };
            gate(rows, fail, GitRole::Modified, "Changes requested", &who);
            blockers.push((
                if who.is_empty() { "Changes were requested".into() } else { who },
                "push fixes, then ask for another review".into(),
            ));
        }
        "REVIEW_REQUIRED" => {
            gate(rows, wait, GitRole::Modified, "Review required", "an approving review is needed");
            blockers.push(("No approving review yet".into(), "request a reviewer from the Overview".into()));
        }
        _ => gate(rows, wait, GitRole::Muted, "No review decision", ""),
    }
    let counts = record.check_counts();
    let (_, ci, noun) = workspace.ci_words();
    match counts.state() {
        Some(CiState::Passed) | Some(CiState::Skipped) => gate(rows, pass, GitRole::Added, &format!("{ci} passed"), &counts.summary()),
        Some(CiState::Failed) | Some(CiState::Cancelled) => {
            gate(rows, fail, GitRole::Removed, &format!("{ci} failed"), &counts.summary());
            let failed = record
                .checks
                .iter()
                .filter(|check| matches!(check.state, CiState::Failed | CiState::Cancelled))
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>();
            let named = match failed.as_slice() {
                [] => format!("{ci} failed"),
                [one] => format!("{one} failed"),
                [one, rest @ ..] => format!("{one} and {} more failed", rest.len()),
            };
            blockers.push((named, format!("open the {} tab for logs", workspace.ci_words().0)));
        }
        Some(_) => {
            gate(rows, wait, GitRole::Modified, &format!("{ci} running"), &counts.summary());
            blockers.push((format!("{ci} still running"), "wait, or R to refresh".into()));
        }
        None => gate(rows, wait, GitRole::Muted, &format!("No {noun} reported"), ""),
    }
    let threads = threads_state(workspace, id);
    match threads {
        Some((0, _)) => gate(rows, pass, GitRole::Added, "Conversations resolved", ""),
        Some((open, _)) => {
            let what = format!("{open} unresolved conversation{}", if open == 1 { "" } else { "s" });
            gate(rows, "○", GitRole::Modified, &what, "resolve them in Activity");
            blockers.push((what, "resolve them in Activity".into()));
        }
        None => gate(rows, wait, GitRole::Muted, "Conversations loading", ""),
    }
    let required = record.checks.iter().filter(|check| check.required).collect::<Vec<_>>();
    if !required.is_empty() {
        rows.push(blank());
        rows.push(heading(&format!("Required {noun}"), ""));
        for check in required {
            let (mark, _, tone) = ci_mark(check.state, mode);
            let mut row = Rich::new()
                .plain("  ")
                .push(&format!("{mark} "), role(tone))
                .push(&check.name, role(GitRole::Text))
                .plain("  ")
                .push(&check.word, role(tone));
            let mut action = None;
            if let Some(run) = check.run {
                row = row.plain("  ").push(&format!("Open logs {}", glyph(Glyph::Link, mode)), role(GitRole::Link));
                action = Some(Action::Run(run));
            }
            let mut row = row.line();
            row.action = action;
            rows.push(row);
        }
    }
    rows.push(blank());
    let ready = record.row.lifecycle == Lifecycle::Open
        && record.merge_state == "CLEAN"
        && !matches!(decision, "CHANGES_REQUESTED" | "REVIEW_REQUIRED")
        && threads.is_some_and(|(open, _)| open == 0);
    if ready {
        let mut row = Rich::new()
            .chip("Enter Merge", strong(GitRole::OnFocus))
            .plain("  ")
            .push(
                &format!("pinned to {}", record.head_sha.chars().take(7).collect::<String>()),
                role(GitRole::Muted),
            )
            .line();
        if let Some((range, style)) = row.paint.first_mut() {
            let _ = range;
            *style = crate::ui::Style::Git {
                role: GitRole::OnFocus,
                surface: crate::ui::GitSurface::ChipActive,
                emphasis: crate::ui::Emphasis::Bold,
            };
        }
        row.action = Some(Action::Merge(id, record.head_sha.clone()));
        rows.push(row);
    } else {
        rows.push(
            Rich::new()
                .chip("Merge unavailable", role(GitRole::Muted))
                .plain("  ")
                .push(
                    &match blockers.len() {
                        0 => "the forge has not cleared it yet".to_owned(),
                        1 => "1 gate blocks it".to_owned(),
                        n => format!("{n} gates block it"),
                    },
                    role(GitRole::Muted),
                )
                .line(),
        );
        if let Some((range, style)) = rows.last_mut().and_then(|row| row.paint.first_mut()) {
            let _ = range;
            *style = crate::ui::Style::Git {
                role: GitRole::Muted,
                surface: crate::ui::GitSurface::Chip(crate::ui::GitChip::Muted),
                emphasis: crate::ui::Emphasis::None,
            };
        }
        if !blockers.is_empty() {
            rows.push(blank());
            rows.push(heading("What blocks it", ""));
            for (index, (what, fix)) in blockers.iter().enumerate() {
                rows.push(
                    Rich::new()
                        .push(&format!("  {}. ", index + 1), role(GitRole::Muted))
                        .push(what, strong(GitRole::Text))
                        .line(),
                );
                rows.push(Rich::new().plain("     ").push(&format!("→ {fix}"), role(GitRole::Muted)).line());
            }
        }
    }
    // What a merge would do, so the pane reads whole even before it is allowed.
    rows.push(blank());
    rows.push(heading("On merge", ""));
    let column = 12;
    if !record.head.is_empty() {
        rows.push(
            field("Lands", column)
                .push(&record.head, role(GitRole::Ref))
                .push(" → ", role(GitRole::Muted))
                .push(base, role(GitRole::Ref))
                .line(),
        );
    }
    let commits = record.commits.len();
    if commits > 0 {
        let mut row = field("Brings", column)
            .push(&format!("{commits} commit{}", if commits == 1 { "" } else { "s" }), role(GitRole::Text));
        if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions) {
            row = row
                .sep()
                .push(&format!("+{add}"), role(GitRole::Added))
                .plain(" ")
                .push(&format!("−{del}"), role(GitRole::Removed));
        }
        rows.push(row.line());
    }
    if !record.head_sha.is_empty() {
        rows.push(
            field("Pinned to", column)
                .push(&record.head_sha.chars().take(7).collect::<String>(), role(GitRole::Hash))
                .push(" · a newer push cancels the merge", role(GitRole::Muted))
                .line(),
        );
    }
}
