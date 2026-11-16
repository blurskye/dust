//! F-PRINBOX: the pull request reader's tabs (DESIGN.md §3.26, §3.27, §3.32) as rows built
//! from typed records: Conversation (`conversation.rs`, ending in the merge box here),
//! Commits and Checks. The Files tab keeps the diff rows of
//! `lines::files` (§3.28), painted on the shared diff surfaces.
use super::super::shell::glyphs::{Glyph, glyph};
use super::issue;
use super::prose::{self, Rich, blank, field, heading, muted, role, strong};
use super::lines::Line;
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::{CiState, Lifecycle, ReviewState};
use crate::forge::records::{self, CheckCounts, DetailRecord, TimelineEntry};
use crate::ui::GitRole;

/// GitHub's four tabs (DESIGN.md §3.26): the Conversation holds what Overview, Activity and
/// Merge used to (the timeline, the threads, the merge box at its end).
pub(super) const CONVERSATION: usize = 0;
pub(super) const COMMITS: usize = 1;
pub(super) const CHECKS: usize = 2;
pub(super) const FILES: usize = 3;
/// Tabs `H`/`L` and `[`/`]` cycle through; `m` goes to the merge box.
pub(super) const CYCLE: usize = 4;

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
        ReviewState::Approved => (glyph(Glyph::Approved, mode), "Approved", GitRole::Added),
        ReviewState::ChangesRequested => (glyph(Glyph::ChangesRequested, mode), "Changes requested", GitRole::Removed),
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

pub(super) fn counts(workspace: &Workspace, id: u64, record: &DetailRecord) -> [Option<usize>; 4] {
    // Comments, reviews and review comments, as GitHub's Conversation tab counts them.
    let conversation = workspace
        .value(&Key::Timeline(id))
        .map(|value| super::conversation::count(workspace, id, &timeline(value, record)));
    // Every tab counts once its data is here, zero included (`counted`, paint.rs).
    let loaded = workspace.value(&Key::Item(2, id)).is_some();
    [
        conversation,
        // The commits listed, else the count the header's sentence states.
        Some(record.commits.len()).filter(|n| *n > 0).or(Some(0).filter(|_| loaded)),
        // The tab counts what it is named after (r12 gitlab-02): GitLab's `Pipelines` counts
        // pipelines, not the jobs in them; `Checks` counts checks. A merge request's jobs come
        // from its head pipeline, so jobs whose link names no pipeline count as that one.
        Some(if workspace.terms().run_noun == "pipeline" {
            record
                .checks
                .iter()
                .map(|check| check.run)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        } else {
            record.checks.len()
        })
        .filter(|_| loaded),
        // The overview's "N files changed" and this count come from the same place: the
        // listed files, else the forge's changed-file total.
        Some(record.files.len())
            .filter(|n| *n > 0)
            .or(record.row.changed_files.map(|n| n as usize))
            .or(Some(0).filter(|_| loaded)),
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
    let record = workspace.checked(DetailRecord::from_value(value));
    let now = records::now();
    if loaded.is_none() {
        if let Some(Err(error)) = workspace.cache.get(&key) {
            super::issue::unavailable(workspace, &mut rows, error, "R");
        } else {
            rows.push(muted("Loading details…"));
        }
        rows.push(blank());
        // A list row that omitted the body says nothing about it: no "No description".
        // Indented as the description card's body, so the text stays put when it loads.
        if workspace.pr_tab == CONVERSATION && !value["body"].is_null() {
            prose::markdown(&mut rows, &record.body, width, "  ");
        }
        return rows;
    }
    let item = loaded.expect("loaded");
    // Every `+N −M` on the page counts the diff's hunks once it is here (R16 prs-02).
    let record = super::files::diff_counted(workspace, id, &record);
    match workspace.pr_tab {
        COMMITS => commits(workspace, &record, &mut rows, width, now, mode),
        CHECKS => checks(workspace, id, &record, &mut rows, workspace.ci_words().2, width, mode),
        FILES => super::files::rows(workspace, id, item, &record, &mut rows, width, mode),
        _ => {
            let entries = workspace
                .value(&Key::Timeline(id))
                .map(|value| timeline(value, &record))
                .unwrap_or_default();
            super::conversation::pull(workspace, id, &record, entries, &mut rows, width, now, mode);
            merge(workspace, id, item, &record, &mut rows, mode);
        }
    }
    rows
}

fn loading(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            let words = workspace.explain(error);
            rows.push(Rich::new().push("✕ ", role(GitRole::Removed)).push(&words.headline, role(GitRole::Removed)).line());
            rows.push(muted(&words.hint));
            true
        }
        None => {
            rows.push(muted("Loading…"));
            true
        }
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
    let head_sha = record.commits.last().map(|commit| commit.sha.clone());
    let ci = record.check_counts().state();
    // Beside its list the tab shows a commit, the newest until another is picked, as Files
    // changed shows its first file (grade14 GH-*-11); a Narrow reader lists them first.
    let beside = super::files::outline_beside(workspace, width);
    let open = workspace
        .selected_commit
        .as_ref()
        .and_then(|sha| record.commits.iter().find(|commit| commit.sha == *sha))
        .or_else(|| if beside { record.commits.last() } else { None });
    // GitHub's Commits tab (§3.26): the full-width list grouped by day, each row its whole
    // subject, author, age and sha. Beside it (any reader but a Narrow one) the open commit's
    // diff follows below the list rather than a 25-cell rail cutting every subject (r15 PR-05).
    let Some(commit) = open else {
        commit_list(record, rows, width, now, mode, None);
        commit_summary(record, rows, now);
        return;
    };
    if beside {
        commit_list(record, rows, width, now, mode, Some(&commit.sha));
        rows.push(blank());
    }
    // One commit open: its diff, as GitHub's commit page. Alone in a Narrow reader, Enter on
    // `‹ All commits` goes back to the list; beside it the list is there already.
    if !beside {
        let mut back = Rich::new()
            .push("‹ ", role(GitRole::Focus))
            .push("All commits", role(GitRole::Link))
            .line();
        back.action = Some(Action::Commit(commit.sha.clone()));
        rows.push(back);
        rows.push(blank());
    }
    let mut header = Rich::new()
        .push(&format!(" {} ", glyph(Glyph::CommitHead, mode)), role(GitRole::Ref))
        .push(&commit.headline, strong(GitRole::Text))
        .line();
    header.band = Some(crate::ui::GitSurface::FileBand);
    rows.push(header);
    let mut facts = Rich::new().plain("  ");
    if !commit.author.is_empty() {
        facts = facts.push(&commit.author, role(GitRole::Author)).plain(" ").push("committed", role(GitRole::Muted));
    }
    let when = fresh::relative(&commit.committed, now);
    if !when.is_empty() {
        facts = facts.plain(" ").push(&when, role(GitRole::Muted));
    }
    facts = facts.sep().push(&commit.sha.chars().take(7).collect::<String>(), role(GitRole::Hash));
    if head_sha.as_deref() == Some(commit.sha.as_str())
        && let Some(state) = ci
    {
        let (mark, word, tone) = ci_mark(state, mode);
        facts = facts.sep().push(&format!("{mark} {word}"), role(tone));
    }
    rows.push(facts.line());
    let sha = &commit.sha;
    rows.push(blank());
    let key = Key::Commit(sha.clone());
    if !loading(workspace, &key, rows) {
        commit_diff(workspace, &key, rows);
    }
}

/// The Commits tab's list (§3.26): grouped by day, newest day last as the branch grew; each
/// row its subject, author, age, sha and, on the head, the checks' mark. The `open` commit's
/// subject is drawn in the focus colour while its diff shows below.
fn commit_list(record: &DetailRecord, rows: &mut Vec<Line>, width: usize, now: i64, mode: ExplorerIconMode, open: Option<&str>) {
    let head_sha = record.commits.last().map(|commit| commit.sha.as_str());
    let ci = record.check_counts().state();
    let headline_width = width.saturating_sub(34).max(16);
    let mut day = String::new();
    for commit in &record.commits {
        let on = commit_day(&commit.committed, now);
        if on != day {
            if !day.is_empty() {
                rows.push(blank());
            }
            let title = if on.is_empty() { "Commits".to_owned() } else { format!("Commits on {on}") };
            rows.push(Rich::new().push(&format!("{} ", glyph(Glyph::ForgeCommit, mode)), role(GitRole::Muted)).push(&title, strong(GitRole::Muted)).line());
            day = on;
        }
        let head = head_sha == Some(commit.sha.as_str());
        let is_open = open == Some(commit.sha.as_str());
        let short = commit.sha.chars().take(7).collect::<String>();
        let headline = super::super::shell::chrome::ellipsize(&commit.headline, headline_width);
        let mut row = Rich::new()
            .push("  │ ", role(if is_open { GitRole::Focus } else { GitRole::Border }))
            .push(&headline, if is_open { strong(GitRole::Focus) } else { role(GitRole::Text) })
            .to_column(headline_width + 5);
        if !commit.author.is_empty() {
            row = row.push(&commit.author, role(GitRole::Author));
        }
        let when = fresh::relative(&commit.committed, now);
        if !when.is_empty() {
            // `author · age`, or the age alone when the forge names no author.
            if !commit.author.is_empty() {
                row = row.sep();
            }
            row = row.push(&when, role(GitRole::Muted));
        }
        let row = row.plain("  ");
        let row = match ci.filter(|_| head) {
            Some(state) => {
                let (mark, _, tone) = ci_mark(state, mode);
                row.push(&format!("{mark} "), role(tone))
            }
            None => row.plain("  "),
        };
        let mut row = row.push(&short, role(GitRole::Hash)).line();
        row.action = Some(Action::Commit(commit.sha.clone()));
        rows.push(row);
    }
}

/// An open commit's diff, as the Git screen's preview reads it: each file's header carries
/// its mode change (`new file`), a blank row parts the files, and every changed line has the
/// same `old new ±` gutter (grade14 GH-gitlab-02).
fn commit_diff(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) {
    let preview = super::super::preview::Preview::git(workspace.raw(key).unwrap_or_default(), None);
    let number = preview.number_width();
    let mut notes: Vec<String> = Vec::new();
    let mut files = 0;
    let mut raw = false;
    for source in preview.rows {
        let header = source.style == crate::ui::Style::GitHeading;
        if source.marker.is_none() && !header && !source.text.starts_with("@@") {
            // A patch's own header lines (`new file mode 100644`, a rename): the file's row
            // says them once.
            let text = source.text.trim();
            // A patch with no hunks (a binary file) comes whole, its `diff --git` included:
            // its lines stay as they are, up to the next file's header.
            raw |= text.starts_with("diff --git ");
            if raw {
                rows.extend(notes.drain(..).map(|note| muted(&note)));
                rows.push(muted(text));
                continue;
            }
            let note = if text.starts_with("new file mode") {
                "new file".to_owned()
            } else if text.starts_with("deleted file mode") {
                "deleted".to_owned()
            } else if let Some(from) = text.strip_prefix("rename from ") {
                format!("renamed from {from}")
            } else if text.is_empty() || text.starts_with("similarity index") || text.starts_with("rename to ") {
                continue;
            } else {
                text.to_owned()
            };
            notes.push(note);
            continue;
        }
        if header {
            raw = false;
            if files > 0 {
                rows.push(blank());
            }
            files += 1;
            let mut text = source.text.clone();
            for note in notes.drain(..) {
                text.push_str(" · ");
                text.push_str(&note);
            }
            let mut row = Line::text(&text, source.style);
            row.band = Some(crate::ui::GitSurface::FileBand);
            rows.push(row);
            continue;
        }
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
                "{:>number$} {:>number$} {marker} ",
                source.old.map(|n| n.to_string()).unwrap_or_default(),
                source.new.map(|n| n.to_string()).unwrap_or_default()
            );
        }
        rows.push(row);
    }
    rows.extend(notes.iter().map(|note| muted(note)));
}

/// `Nov 14`, or `Nov 14, 2025` in another year than `now`'s: the day a commit was made.
fn commit_day(stamp: &str, now: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let Some(date) = stamp.trim().get(..10) else {
        return String::new();
    };
    let mut parts = date.split('-').map(|part| part.parse::<usize>().ok());
    let (Some(Some(year)), Some(Some(month)), Some(Some(day))) = (parts.next(), parts.next(), parts.next()) else {
        return String::new();
    };
    let Some(name) = month.checked_sub(1).and_then(|at| MONTHS.get(at)) else {
        return String::new();
    };
    // Days since the epoch to a civil year (Howard Hinnant's algorithm), for `now`'s year.
    let days = now.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let this_year = yoe + era * 400 + i64::from(mp >= 10);
    if year as i64 == this_year {
        format!("{name} {day}")
    } else {
        format!("{name} {day}, {year}")
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
fn commit_summary(record: &DetailRecord, rows: &mut Vec<Line>, now: i64) {
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
            row.push(&format!("committed {}", fresh::relative(&newest.1.committed, now)), role(GitRole::Text))
        } else {
            row.push(&format!("over {}", span_words(span)), role(GitRole::Text))
                .sep()
                .push(&format!("newest {}", fresh::relative(&newest.1.committed, now)), role(GitRole::Muted))
        };
        rows.push(row.line());
    }
    if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions) {
        let files = if record.files.is_empty() { record.row.changed_files.unwrap_or(0) as usize } else { record.files.len() };
        let mut row = field("Changes", column).diffstat(add, del);
        if files > 0 {
            row = row.push(&format!(" in {files} file{}", if files == 1 { "" } else { "s" }), role(GitRole::Muted));
        }
        rows.push(row.line());
    }
    rows.push(blank());
    rows.push(muted("Enter on a commit previews its diff here"));
}

/// Failing first, then running, passing, and skipped last (§3.29): the first screen of a
/// long list answers "what broke", not "what was skipped".
pub(super) fn check_rank(state: CiState) -> u8 {
    match state {
        CiState::Failed | CiState::Cancelled => 0,
        CiState::Pending | CiState::ActionRequired => 1,
        CiState::Passed => 2,
        CiState::Unknown => 3,
        CiState::Skipped => 4,
    }
}

/// A duration worth showing: forges report `0` for skipped and never-started checks.
pub(super) fn shown_duration(check: &records::CheckRow) -> Option<u64> {
    check.duration.filter(|seconds| *seconds > 0)
}

/// `Run integration…r-latest)`: a long check name keeps both ends, since the end often
/// tells otherwise-identical jobs apart; `tail` cells of it after the `…`.
fn middle_at(text: &str, cells: usize, tail: usize) -> String {
    use super::super::shell::chrome::{ellipsize, ellipsize_start};
    let width = unicode_width::UnicodeWidthStr::width(text);
    if width <= cells || cells < 9 {
        return ellipsize(text, cells);
    }
    let tail = tail.clamp(1, cells - 2);
    let head = cells - 1 - tail;
    format!(
        "{}…{}",
        ellipsize(text, head + 1).trim_end_matches('…'),
        ellipsize_start(text, tail + 1).trim_start_matches('…')
    )
}

/// A name ending in `(param)` cut inside its parentheses, never across them, so no `)` is
/// left without its `(` (r12 gitea-05): `Run integration tests with Docker (…-latest)`.
fn paren_cut(text: &str, cells: usize) -> Option<String> {
    use super::super::shell::chrome::{ellipsize, ellipsize_start};
    use unicode_width::UnicodeWidthStr;
    if text.width() <= cells || !text.ends_with(')') {
        return None;
    }
    let open = text.rfind(" (")?;
    let base = &text[..open];
    let param = &text[open + 2..text.len() - 1];
    let base_width = base.width();
    if base.is_empty() || param.is_empty() {
        return None;
    }
    if base_width + 6 <= cells {
        let kept = ellipsize_start(param, cells - base_width - 3);
        return Some(format!("{base} (…{})", kept.trim_start_matches('…')));
    }
    // Too long to keep the base whole (r13, 100 columns): cut the base at its end and keep
    // the parameter's tail, which tells sibling jobs apart, still inside its own `( )`:
    // `Run integration tests w… (…ker-stable)`.
    let param_cells = (param.width() + 1).min(11);
    let base_cells = cells.checked_sub(3 + param_cells).filter(|n| *n >= 6)?;
    let kept = if param.width() + 1 <= param_cells {
        param.to_owned()
    } else {
        format!("…{}", ellipsize_start(param, param_cells).trim_start_matches('…'))
    };
    Some(format!("{} ({kept})", ellipsize(base, base_cells).trim_end()))
}

fn middle(text: &str, cells: usize) -> String {
    paren_cut(text, cells).unwrap_or_else(|| middle_at(text, cells, cells.saturating_sub(1) / 3))
}

/// Names cut to `cells` that stay as distinct as they were (r9 R8-03): the cut slides along
/// the name until no two different names read the same; failing that, each shows the part
/// where it starts to differ from its neighbours.
pub(super) fn distinct(names: &[&str], cells: usize) -> Vec<String> {
    let unique = |shown: &[String]| {
        (0..names.len()).all(|a| (a + 1..names.len()).all(|b| names[a] == names[b] || shown[a] != shown[b]))
    };
    // First the cut that keeps parentheses whole; the sliding cut only where that one
    // would make two names read the same.
    let whole = names.iter().map(|name| middle(name, cells)).collect::<Vec<_>>();
    if unique(&whole) {
        return whole;
    }
    let first = cells.saturating_sub(1) / 3;
    let tails = (first..cells.saturating_sub(1)).chain((1..first).rev());
    for tail in std::iter::once(first).chain(tails) {
        let shown = names.iter().map(|name| middle_at(name, cells, tail)).collect::<Vec<_>>();
        if unique(&shown) {
            return shown;
        }
    }
    names
        .iter()
        .map(|name| {
            let common = names
                .iter()
                .filter(|other| *other != name)
                .map(|other| name.chars().zip(other.chars()).take_while(|(a, b)| a == b).count())
                .max()
                .unwrap_or(0);
            let from = name.char_indices().nth(common.saturating_sub(cells / 3)).map_or(name.len(), |(at, _)| at);
            if from == 0 {
                middle(name, cells)
            } else {
                super::super::shell::chrome::ellipsize(&format!("…{}", &name[from..]), cells)
            }
        })
        .collect()
}

/// A check's workflow and its own name: the forge's workflow, else the `workflow / ` that
/// Gitea, Forgejo and GitHub's status contexts lead the name with.
pub(super) fn check_parts(check: &records::CheckRow) -> (&str, &str) {
    let name = check.name.as_str();
    if !check.workflow.is_empty() {
        let own = name.strip_prefix(check.workflow.as_str()).and_then(|rest| rest.strip_prefix(" / "));
        return (check.workflow.as_str(), own.unwrap_or(name));
    }
    match name.split_once(" / ") {
        Some((workflow, own)) if !workflow.is_empty() && !own.is_empty() => (workflow, own),
        _ => ("", name),
    }
}

/// The events CI runs on, as Gitea, Forgejo and GitHub status contexts end with them
/// (`build (pull_request)`). A trailing `(docker-latest)` is a matrix value, not an event.
const EVENTS: [&str; 14] = [
    "pull_request",
    "pull_request_target",
    "pull_request_review",
    "push",
    "merge_group",
    "workflow_dispatch",
    "workflow_run",
    "workflow_call",
    "schedule",
    "release",
    "issue_comment",
    "repository_dispatch",
    "check_run",
    "check_suite",
];

/// A check's own name and the event it ends with (r10 R9-03): the event goes in its
/// group's heading, so the row keeps only the name.
pub(super) fn split_event(own: &str) -> (&str, Option<&str>) {
    if let Some(at) = own.rfind(" (").filter(|at| *at > 0 && own.ends_with(')')) {
        let event = &own[at + 2..own.len() - 1];
        if EVENTS.contains(&event) {
            return (&own[..at], Some(event));
        }
    }
    (own, None)
}

/// The key a folded group of skipped checks is opened under, in the reader's fold set.
pub(super) fn skipped_key(workflow: &str, event: Option<&str>) -> String {
    format!("\u{0}skipped\u{0}{workflow}\u{0}{}", event.unwrap_or(""))
}

/// The Checks tab (`checks.rs`): the job list beside the selected job's steps and log.
fn checks(
    workspace: &Workspace,
    id: u64,
    record: &DetailRecord,
    rows: &mut Vec<Line>,
    noun: &str,
    width: usize,
    mode: ExplorerIconMode,
) {
    super::checks::rows(workspace, id, record, rows, noun, width, mode);
}

/// A check's state in the screen's own words: `failed`/`passed`, never the forge's raw
/// `failure`/`success` (r11 GLCR-08), in the Checks list and the Merge tab alike.
pub(super) fn state_word(word: &str) -> &str {
    match word {
        "success" => "passed",
        "failure" => "failed",
        "in progress" | "in_progress" | "queued" => "running",
        word => word,
    }
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
    // The merge box ends the Conversation, as on GitHub: one card, its header the place `m`
    // goes to.
    let start = rows.len();
    let mut header = Rich::new()
        .push(&format!(" {} ", glyph(Glyph::PullRequest, mode)), role(GitRole::Muted))
        .push(if record.row.lifecycle == Lifecycle::Merged { "Merged" } else { "Merge" }, strong(GitRole::Text))
        .line();
    header.band = Some(crate::ui::GitSurface::FileBand);
    header.card = true;
    header.merge_box = true;
    rows.push(header);
    let readiness = rows.len();
    rows.push(blank());
    // Each gate that holds the merge back, with what clears it, for the card below, and the
    // row its gate took above (r9 R8-07: a blocker is stated once, in the numbered list).
    let mut blockers: Vec<(usize, String, String, Option<Action>)> = Vec::new();
    let base = if record.base.is_empty() { "the base branch" } else { record.base.as_str() };
    match record.row.lifecycle {
        Lifecycle::Merged => gate(rows, glyph(Glyph::PullMerged, mode), GitRole::Ref, "Merged", "nothing left to do"),
        Lifecycle::Closed | Lifecycle::ClosedNotPlanned => {
            gate(rows, fail, GitRole::Removed, "Closed", "reopen with X before merging");
            blockers.push((rows.len() - 1, "It is closed".into(), "X reopens it".into(), None));
        }
        Lifecycle::Draft => {
            gate(rows, wait, GitRole::Muted, "Draft", "mark it ready for review first");
            blockers.push((rows.len() - 1, "It is a draft".into(), "mark it ready for review".into(), None));
        }
        Lifecycle::Open => {}
    }
    match record.mergeable.as_str() {
        // A fact about conflicts only, never "can merge": other gates may still block it (r11).
        "MERGEABLE" => gate(rows, pass, GitRole::Added, &format!("No conflicts with {base}"), ""),
        "CONFLICTING" => {
            gate(rows, fail, GitRole::Removed, "Conflicts", &format!("resolve conflicts with {base} first"));
            blockers.push((rows.len() - 1, format!("Conflicts with {base}"), format!("merge or rebase {base} into the branch"), None));
        }
        // An unknown answer is not a gate: it would sit above `Merge unavailable` as a second
        // headline (r11 GL-03). The chip below says the forge is still checking instead.
        _ => {}
    }
    let checking = !matches!(record.mergeable.as_str(), "MERGEABLE" | "CONFLICTING");
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
            gate(rows, glyph(Glyph::ChangesRequested, mode), GitRole::Modified, "Changes requested", &who);
            blockers.push((rows.len() - 1,
                if who.is_empty() { "Changes were requested".into() } else { who },
                "push fixes, then ask for another review".into(),
                None,
            ));
        }
        "REVIEW_REQUIRED" => {
            // Asked already: the remedy is to wait on them, not to ask again (r11 prs-06).
            let reviewers = record.reviewers();
            let asked = reviewers
                .iter()
                .filter(|(_, state)| *state == ReviewState::Requested)
                .map(|(login, _)| login.clone())
                .collect::<Vec<_>>();
            let approved = reviewers.iter().filter(|(_, state)| *state == ReviewState::Approved).count();
            // An approval the timeline and sidebar show is counted, never denied (grade14
            // GH-github-02): `1 of 2 approvals · waiting on sam`.
            let what = match approved {
                0 => "No approving review yet".to_owned(),
                n if asked.is_empty() => format!("{n} approval{}, another is required", if n == 1 { "" } else { "s" }),
                n => format!("{n} of {} approvals", n + asked.len()),
            };
            let detail = if approved == 0 { "an approving review is needed".to_owned() } else { what.clone() };
            gate(rows, wait, GitRole::Modified, "Review required", &detail);
            let fix = if asked.is_empty() {
                "request a reviewer".to_owned()
            } else {
                format!("waiting on {}", asked.join(", "))
            };
            blockers.push((rows.len() - 1, what, fix, None));
        }
        _ => gate(rows, wait, GitRole::Muted, "No review decision", ""),
    }
    let counts = record.check_counts();
    let (_, ci, noun) = workspace.ci_words();
    match counts.state() {
        // `Checks passed`, not `Checks passed · 3 passed` (r11): only skips add a fact.
        Some(CiState::Passed) | Some(CiState::Skipped) => {
            let skipped = if counts.skipped > 0 { format!("{} skipped", counts.skipped) } else { String::new() };
            gate(rows, pass, GitRole::Added, &format!("{ci} passed"), &skipped)
        }
        Some(CiState::Failed) | Some(CiState::Cancelled) => {
            gate(rows, fail, GitRole::Removed, &format!("{ci} failed"), &counts.summary());
            let failing = record
                .checks
                .iter()
                .filter(|check| matches!(check.state, CiState::Failed | CiState::Cancelled))
                .collect::<Vec<_>>();
            // The blocker is the one place a failing check is named; the Required table
            // below leaves it out (r11 GL-02: never stated twice).
            let required = failing.iter().any(|check| check.required);
            let one = noun.strip_suffix('s').unwrap_or(noun);
            let named = match failing.as_slice() {
                [] => format!("{ci} failed"),
                [check] if required => format!("Required {one} {} failed", check.name),
                [check] => format!("{} failed", check.name),
                [check, rest @ ..] => format!("{} and {} more failed", check.name, rest.len()),
            };
            let (fix, action) = match failing.as_slice() {
                [check] if check.run.is_some() => {
                    (format!("Open logs {}", glyph(Glyph::Link, mode)), check.run.map(Action::Run))
                }
                _ => (format!("the {} tab has the logs", workspace.ci_words().0), None),
            };
            blockers.push((rows.len() - 1, named, fix, action));
        }
        Some(_) => {
            gate(rows, wait, GitRole::Modified, &format!("{ci} running"), &counts.summary());
            blockers.push((rows.len() - 1, format!("{ci} still running"), "wait, or R to refresh".into(), None));
        }
        None => gate(rows, wait, GitRole::Muted, &super::checks::no_checks(record), ""),
    }
    let threads = threads_state(workspace, id);
    match threads {
        Some((0, _)) => gate(rows, pass, GitRole::Added, "Conversations resolved", ""),
        Some((open, _)) => {
            let what = format!("{open} unresolved conversation{}", if open == 1 { "" } else { "s" });
            // Where the first one is, as data (`orbit/queue.py:14 · alex`); `n` is the
            // footer's key, not a blocker's line (R16 prs-05).
            let first = workspace
                .value(&Key::Threads(id))
                .map(records::threads)
                .unwrap_or_default()
                .into_iter()
                .find(|thread| thread.resolved == Some(false));
            let place = first.map_or_else(
                || format!("on the {} tab", "Files changed"),
                |thread| {
                    let at = thread.line.map_or_else(|| thread.path.clone(), |line| format!("{}:{line}", thread.path));
                    match thread.comments.first() {
                        Some(comment) if !comment.author.is_empty() => format!("{at} · {}", comment.author),
                        _ => at,
                    }
                },
            );
            let place = if open > 1 { format!("first at {place}") } else { place };
            gate(rows, "◔", GitRole::Modified, &what, &place);
            blockers.push((rows.len() - 1, what, place, None));
        }
        None => gate(rows, wait, GitRole::Muted, "Conversations loading", ""),
    }
    // Every required check, failing ones included, from the same checks the blockers read
    // (r12 gitlab-01: a list of the passing ones read as "all required passed").
    let required = record.checks.iter().filter(|check| check.required).collect::<Vec<_>>();
    if !required.is_empty() {
        rows.push(blank());
        rows.push(heading(&format!("Required {noun}"), ""));
        // Names in one column, so each state sits under the last (no stray double space).
        let column = required.iter().map(|check| unicode_width::UnicodeWidthStr::width(check.name.as_str())).max().unwrap_or(0) + 2;
        for check in required {
            let (mark, _, tone) = ci_mark(check.state, mode);
            let row = Rich::new()
                .plain("  ")
                .push(&format!("{mark} "), role(tone))
                .push(&check.name, role(GitRole::Text))
                .to_column(column + 4)
                .push(state_word(&check.word), role(tone));
            // Enter opens the run, as on the Checks tab; the footer names the key.
            let mut row = row.line();
            row.action = check.run.map(Action::Run);
            rows.push(row);
        }
    }
    let ready = record.row.lifecycle == Lifecycle::Open
        && record.merge_state == "CLEAN"
        && !matches!(decision, "CHANGES_REQUESTED" | "REVIEW_REQUIRED")
        && threads.is_some_and(|(open, _)| open == 0);
    if !ready {
        for (at, ..) in blockers.iter().rev() {
            rows.remove(*at);
        }
        // With every gate moved to the list below, the heading keeps one blank, not two
        // (r12: GitLab's Merge tab opened on an empty double gap).
        while rows.get(readiness).is_some_and(|row| row.text.is_empty())
            && rows.get(readiness + 1).is_some_and(|row| row.text.is_empty())
        {
            rows.remove(readiness);
        }
    }
    rows.push(blank());
    if ready {
        let mut row = Rich::new()
            .chip("Merge pull request", strong(GitRole::OnFocus))
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
        let mut row = Rich::new().chip("Merging is blocked", role(GitRole::Muted));
        if blockers.is_empty() {
            row = row.plain("  ").push(
                if checking { "checking for conflicts…" } else { "not ready yet" },
                role(GitRole::Muted),
            );
        }
        rows.push(row.line());
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
            for (index, (_, what, fix, action)) in blockers.iter().enumerate() {
                rows.push(
                    Rich::new()
                        .push(&format!("  {}. ", index + 1), role(GitRole::Muted))
                        .push(what, strong(GitRole::Text))
                        .line(),
                );
                let tone = if action.is_some() { GitRole::Link } else { GitRole::Muted };
                let mut row = Rich::new().plain("     ").push(&format!("→ {fix}"), role(tone)).line();
                row.action = action.clone();
                rows.push(row);
            }
        }
    }
    // What a merge would do, so the pane reads whole even before it is allowed.
    rows.push(blank());
    rows.push(heading("On merge", ""));
    let column = 12;
    // GitHub's method selector: Enter takes the next method this forge allows; nothing is
    // written until the merge is confirmed in its sheet (grade14 GH-*-04).
    let methods = workspace.merge_methods();
    if record.row.lifecycle == Lifecycle::Open && !workspace.read_only() && !methods.is_empty() {
        let mut row = field("Method", column)
            .push(merge_method_words(workspace.merge_method()), role(GitRole::Text));
        if methods.len() > 1 {
            row = row.push(" ▾", role(GitRole::Muted));
        }
        let mut row = row.line();
        row.action = (methods.len() > 1).then_some(Action::MergeMethod(id));
        rows.push(row);
    }
    // The branches are in the header (`main ← fix/backoff`); no `Lands` row repeats them
    // (r11).
    let commits = record.commits.len();
    if commits > 0 {
        let mut row = field("Brings", column)
            .push(&format!("{commits} commit{}", if commits == 1 { "" } else { "s" }), role(GitRole::Text));
        if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions) {
            if add + del > 0 {
                row = row.sep().diffstat(add, del);
            }
        }
        rows.push(row.line());
    }
    // The box's rows sit inside its card; its controls are stops for `j`/`k`.
    for row in &mut rows[start + 1..] {
        super::conversation::indent(row, "  ");
        row.card = row.action.is_some();
    }
}

/// GitHub's words for a merge method.
fn merge_method_words(method: &str) -> &'static str {
    match method {
        "Merge" => "Create a merge commit",
        "Rebase" => "Rebase and merge",
        _ => "Squash and merge",
    }
}

impl Workspace {
    /// The merge methods this forge allows, as the merge sheet names them.
    pub(super) fn merge_methods(&self) -> Vec<&'static str> {
        use crate::forge::model::MergeMethod;
        self.capabilities()
            .merge_methods
            .iter()
            .filter_map(|method| match method {
                MergeMethod::Squash => Some("Squash"),
                MergeMethod::Merge => Some("Merge"),
                MergeMethod::Rebase => Some("Rebase"),
                _ => None,
            })
            .collect()
    }

    /// The method a merge would use: the one chosen in the merge box, else squash where the
    /// forge allows it, else its first.
    pub(super) fn merge_method(&self) -> &'static str {
        let methods = self.merge_methods();
        self.merge_method
            .filter(|chosen| methods.contains(chosen))
            .or_else(|| methods.contains(&"Squash").then_some("Squash"))
            .or_else(|| methods.first().copied())
            .unwrap_or("Merge")
    }

    /// The merge box's `Method` row: the next allowed method, round to the first.
    pub(super) fn cycle_merge_method(&mut self) {
        let methods = self.merge_methods();
        let current = self.merge_method();
        let at = methods.iter().position(|method| *method == current).map_or(0, |at| (at + 1) % methods.len().max(1));
        self.merge_method = methods.get(at).copied();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// r9 R8-03: no two check rows read the same when their names differ, at any width.
    #[test]
    fn check_names_stay_distinguishable_when_cut() {
        let contexts = [
            "checks / Build Forgejo Runner (pull_request)",
            "checks / Build unsupported platforms (pull_request)",
            "checks / orbit compatibility (orbit/orbit:latest) (pull_request)",
            "checks / orbit compatibility (orbit/orbit:main-nightly) (pull_request)",
            "checks / Run exec tests with Docker (docker-latest) (pull_request)",
            "checks / Run integration tests with Docker (docker-latest) (pull_request)",
            "checks / Run integration tests with Docker (docker-stable) (pull_request)",
            "checks / Run integration tests with Podman (pull_request)",
            "checks / validate pre-commit-hooks file (pull_request)",
        ];
        let checks = contexts
            .iter()
            .map(|name| records::CheckRow {
                name: (*name).into(),
                workflow: String::new(),
                state: CiState::Passed,
                word: "success".into(),
                duration: None,
                run: None,
                job: None,
                url: String::new(),
                required: false,
            })
            .collect::<Vec<_>>();
        let parts = checks.iter().map(check_parts).collect::<Vec<_>>();
        assert!(parts.iter().all(|(workflow, _)| *workflow == "checks"), "{parts:?}");
        let split = parts.iter().map(|(_, own)| split_event(own)).collect::<Vec<_>>();
        assert!(split.iter().all(|(_, event)| *event == Some("pull_request")), "{split:?}");
        assert_eq!(split_event("Run tests (docker-latest)"), ("Run tests (docker-latest)", None));
        let own = split.iter().map(|(own, _)| *own).collect::<Vec<_>>();
        for names in [own.clone(), contexts.to_vec()] {
            for cells in 12..=60 {
                let shown = distinct(&names, cells);
                for (a, left) in shown.iter().enumerate() {
                    assert!(unicode_width::UnicodeWidthStr::width(left.as_str()) <= cells, "{cells}: {left}");
                    for (b, right) in shown.iter().enumerate().skip(a + 1) {
                        assert!(names[a] == names[b] || left != right, "{cells}: {} and {} both read {left}", names[a], names[b]);
                    }
                }
            }
        }
        // r13 gitea-05: a cut never leaves a `)` without its `(`, the base cut too when needed.
        for cells in 20..=60 {
            for shown in distinct(&own, cells) {
                assert_eq!(shown.matches('(').count(), shown.matches(')').count(), "{cells}: {shown}");
            }
        }
        assert_eq!(
            middle("Run integration tests with Docker (docker-stable)", 38),
            "Run integration tests w… (…ker-stable)"
        );
    }
}
