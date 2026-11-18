//! Typed records the forge tabs paint: one row of a pull request or issue
//! list, the reader's item detail, checks, review threads and timeline entries.
//!
//! The Workspace cache keeps the gh-shaped JSON `forge::legacy` produces, because the
//! freshness, optimistic-write and inbox machinery edit those values in place. The painters
//! never read that JSON; they read these records, built from it in one place.

use crate::model::{CiState, Lifecycle, ReviewState, ReviewTally};
use serde_json::Value;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn count(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn login(value: &Value) -> String {
    value["author"]["login"]
        .as_str()
        .or_else(|| value["user"]["login"].as_str())
        .or_else(|| value["actor"]["login"].as_str())
        .unwrap_or("ghost")
        .to_owned()
}

// ---------------------------------------------------------------------------------- time

/// Seconds since the Unix epoch for an RFC 3339 stamp (`2026-11-08T10:00:00Z`, offsets and
/// fractions allowed) or a bare date. `None` when it is not one.
pub fn epoch(stamp: &str) -> Option<i64> {
    let stamp = stamp.trim();
    let date = stamp.get(..10)?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days-from-civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut seconds = days * 86_400;
    let rest = &stamp[10..];
    if let Some(time) = rest.strip_prefix(['T', ' ']) {
        let clock = time.get(..8).unwrap_or(time.get(..5)?);
        let mut fields = clock.split(':');
        let hour: i64 = fields.next()?.parse().ok()?;
        let minute: i64 = fields.next()?.parse().ok()?;
        let second: i64 = fields.next().map_or(Some(0), |field| field.parse().ok())?;
        seconds += hour * 3600 + minute * 60 + second;
        let zone = time.find(['Z', 'z', '+']).or_else(|| {
            time.get(1..)
                .and_then(|tail| tail.find('-').map(|at| at + 1))
        });
        if let Some(at) = zone {
            let offset = &time[at..];
            if let Some(sign) = offset
                .chars()
                .next()
                .filter(|sign| matches!(sign, '+' | '-'))
            {
                let digits = offset[1..].replace(':', "");
                if let (Some(h), Some(m)) = (digits.get(..2), digits.get(2..4)) {
                    let shift = h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60;
                    seconds += if sign == '+' { -shift } else { shift };
                }
            }
        }
    }
    Some(seconds)
}

/// Now, for `ago`.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `just now`, `18m ago`, `3h ago`, `2d ago`, then `Nov 8` (and `Nov 8 2024` in another
/// year). Empty when the stamp cannot be read.
pub fn ago(stamp: &str, now: i64) -> String {
    let Some(at) = epoch(stamp) else {
        return String::new();
    };
    let seconds = (now - at).max(0);
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 7 * 86_400 {
        format!("{}d ago", seconds / 86_400)
    } else {
        let month = stamp
            .get(5..7)
            .and_then(|month| month.parse::<usize>().ok())
            .and_then(|month| MONTHS.get(month.wrapping_sub(1)))
            .copied()
            .unwrap_or("");
        let day = stamp
            .get(8..10)
            .and_then(|day| day.parse::<u32>().ok())
            .unwrap_or(0);
        let year = stamp.get(..4).unwrap_or("");
        let this_year = epoch(&format!("{year}-01-01"))
            .is_some_and(|start| now >= start && now < start + 366 * 86_400);
        if this_year {
            format!("{month} {day}")
        } else {
            format!("{month} {day} {year}")
        }
    }
}

// ---------------------------------------------------------------------------------- labels

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LabelChip {
    pub name: String,
    /// The forge's own colour (`d73a4a`), when it sent one.
    pub rgb: Option<[u8; 3]>,
}

pub fn hex(colour: &str) -> Option<[u8; 3]> {
    let colour = colour.trim().trim_start_matches('#');
    if colour.len() != 6 || !colour.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&colour[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

fn labels(value: &Value) -> Vec<LabelChip> {
    list(value, "labels")
        .iter()
        .filter_map(|label| {
            let name = label["name"].as_str().or_else(|| label.as_str())?;
            (!name.is_empty()).then(|| LabelChip {
                name: name.to_owned(),
                rgb: label["color"].as_str().and_then(hex),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------- states

/// Lifecycle from the gh `state` word, `isDraft` and the closed reason.
pub fn lifecycle(value: &Value) -> Lifecycle {
    let reason = value["dusty"]["stateReason"]
        .as_str()
        .or_else(|| value["stateReason"].as_str())
        .unwrap_or("");
    match text(value, "state").to_ascii_uppercase().as_str() {
        "MERGED" => Lifecycle::Merged,
        "CLOSED" if reason.eq_ignore_ascii_case("NOT_PLANNED") => Lifecycle::ClosedNotPlanned,
        "CLOSED" => Lifecycle::Closed,
        _ if value["isDraft"].as_bool() == Some(true) => Lifecycle::Draft,
        _ => Lifecycle::Open,
    }
}

/// A check's word: the conclusion, else a status context's state, else the status.
pub fn check_word(check: &Value) -> &str {
    check["conclusion"]
        .as_str()
        .filter(|value| !value.is_empty())
        .or_else(|| check["state"].as_str().filter(|value| !value.is_empty()))
        .unwrap_or(text(check, "status"))
}

pub fn ci_state(word: &str) -> CiState {
    match word.to_ascii_uppercase().as_str() {
        "SUCCESS" | "COMPLETED" => CiState::Passed,
        "NEUTRAL" | "SKIPPED" => CiState::Skipped,
        "CANCELLED" => CiState::Cancelled,
        "ACTION_REQUIRED" => CiState::ActionRequired,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "STALE" | "STARTUP_FAILURE" => CiState::Failed,
        "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED"
        | "PENDING_DEPLOYMENT" => CiState::Pending,
        _ => CiState::Unknown,
    }
}

/// Counts of a rollup by bucket.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckCounts {
    pub passed: u64,
    pub failed: u64,
    pub pending: u64,
    pub skipped: u64,
    pub unknown: u64,
}

impl CheckCounts {
    pub fn total(&self) -> u64 {
        self.passed + self.failed + self.pending + self.skipped + self.unknown
    }

    pub fn add(&mut self, state: CiState) {
        match state {
            CiState::Passed => self.passed += 1,
            CiState::Failed | CiState::Cancelled | CiState::ActionRequired => self.failed += 1,
            CiState::Pending => self.pending += 1,
            CiState::Skipped => self.skipped += 1,
            CiState::Unknown => self.unknown += 1,
        }
    }

    /// One state for the whole rollup: any failure fails it, then pending, then passed.
    pub fn state(&self) -> Option<CiState> {
        if self.total() == 0 {
            None
        } else if self.failed > 0 {
            Some(CiState::Failed)
        } else if self.pending > 0 {
            Some(CiState::Pending)
        } else if self.passed > 0 {
            Some(CiState::Passed)
        } else if self.skipped > 0 {
            Some(CiState::Skipped)
        } else {
            Some(CiState::Unknown)
        }
    }

    /// `1 failed · 2 passed`, most urgent first.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (count, word) in [
            (self.failed, "failed"),
            (self.pending, "pending"),
            (self.passed, "passed"),
            (self.skipped, "skipped"),
            (self.unknown, "unknown"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }
        parts.join(" · ")
    }
}

fn rollup(value: &Value) -> CheckCounts {
    let mut counts = CheckCounts::default();
    for check in list(value, "statusCheckRollup") {
        counts.add(ci_state(check_word(check)));
    }
    counts
}

fn review(value: &Value) -> Option<ReviewState> {
    match text(value, "reviewDecision") {
        "APPROVED" => Some(ReviewState::Approved),
        "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewState::Requested),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------- rows

/// One record of a pull request or issue list (two rows on screen).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemRecord {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub lifecycle: Lifecycle,
    pub labels: Vec<LabelChip>,
    pub updated: String,
    pub comments: u64,
    /// `+` when the forge stopped counting (gh lists at most 100 comments).
    pub comments_capped: bool,
    /// `owner/name` for cross-repository rows (inbox).
    pub repo: Option<String>,
    pub url: String,
    pub review: Option<ReviewState>,
    pub checks: CheckCounts,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    /// Closed on the forge since the cached list; kept in place until the list is left.
    pub gone: bool,
    pub body: String,
    /// Each reviewer's latest verdict and the open requests, when the list carried them.
    pub tally: Option<ReviewTally>,
    /// When the item was opened; empty when the list did not say.
    pub created: String,
    /// Linked pull requests (an issue) or closing issues (a pull request); 0 when unknown.
    pub linked: u64,
}

impl ItemRecord {
    pub fn from_row(value: &Value) -> Self {
        let comments_list = value["comments"].as_array();
        let comments = comments_list
            .map(|comments| comments.len() as u64)
            .or_else(|| value["commentsCount"].as_u64())
            .or_else(|| value["comments"]["totalCount"].as_u64())
            .unwrap_or(0);
        let extension = &value["dusty"];
        let review = review(value).or_else(|| {
            extension["reviewRequested"]
                .as_bool()
                .filter(|requested| *requested)
                .map(|_| ReviewState::Requested)
        });
        Self {
            number: count(value, "number"),
            title: text(value, "title").to_owned(),
            author: login(value),
            lifecycle: lifecycle(value),
            labels: labels(value),
            updated: text(value, "updatedAt").to_owned(),
            comments,
            comments_capped: comments_list.is_some_and(|comments| comments.len() >= 100),
            repo: value["repository"]["nameWithOwner"]
                .as_str()
                .filter(|repo| !repo.is_empty())
                .map(str::to_owned),
            url: value["url"]
                .as_str()
                .or_else(|| value["html_url"].as_str())
                .unwrap_or("")
                .to_owned(),
            review,
            checks: rollup(value),
            additions: extension["additions"].as_u64(),
            deletions: extension["deletions"].as_u64(),
            changed_files: value["changedFiles"].as_u64(),
            gone: value.get("dustyGone").is_some(),
            body: text(value, "body").to_owned(),
            tally: extension["reviews"].is_object().then(|| ReviewTally {
                approved: extension["reviews"]["approved"].as_u64().unwrap_or(0),
                changes_requested: extension["reviews"]["changesRequested"]
                    .as_u64()
                    .unwrap_or(0),
                pending: extension["reviews"]["pending"].as_u64().unwrap_or(0),
            }),
            created: extension["created"]
                .as_str()
                .or_else(|| value["createdAt"].as_str())
                .unwrap_or("")
                .to_owned(),
            linked: extension["linked"]
                .as_u64()
                .or_else(|| {
                    value["closedByPullRequestsReferences"]
                        .as_array()
                        .map(|links| links.len() as u64)
                })
                .or_else(|| {
                    value["closingIssuesReferences"]
                        .as_array()
                        .map(|links| links.len() as u64)
                })
                .unwrap_or(0),
        }
    }

    pub fn ci(&self) -> Option<CiState> {
        self.checks.state()
    }

    /// The row's review fact: its state, and the words when the counts say more than the
    /// state's own word. `tally` is the list's, else one counted from the opened item.
    pub fn review_fact(&self, tally: Option<ReviewTally>) -> Option<(ReviewState, Option<String>)> {
        let decision = self
            .review
            .filter(|review| *review != ReviewState::NotRequested);
        let Some(tally) = self.tally.or(tally) else {
            return decision.map(|state| (state, None));
        };
        // No decision and nobody asked: the repository requires no review, and the row
        // keeps its room for the state chip (GitHub lists nothing there either).
        if decision.is_none() && tally.pending == 0 {
            return None;
        }
        if decision == Some(ReviewState::ChangesRequested) || tally.changes_requested > 0 {
            return Some((ReviewState::ChangesRequested, None));
        }
        if decision == Some(ReviewState::Approved) && tally.pending == 0 {
            return Some((ReviewState::Approved, None));
        }
        if tally.approved == 0 {
            return decision.map(|state| (state, None));
        }
        let approvals = format!(
            "{} approval{}",
            tally.approved,
            if tally.approved == 1 { "" } else { "s" }
        );
        let words = if tally.pending == 0 {
            approvals
        } else {
            format!("{approvals} · {} pending", tally.pending)
        };
        Some((ReviewState::Approved, Some(words)))
    }
}

/// A pull request's tally from its opened detail: each reviewer's latest verdict, and the
/// requests still open (`DetailRecord::reviewers`).
pub fn tally_of(reviewers: &[(String, ReviewState)]) -> ReviewTally {
    let mut tally = ReviewTally::default();
    for (_, state) in reviewers {
        match state {
            ReviewState::Approved => tally.approved += 1,
            ReviewState::ChangesRequested => tally.changes_requested += 1,
            ReviewState::Requested => tally.pending += 1,
            _ => {}
        }
    }
    tally
}

// ---------------------------------------------------------------------------------- detail

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkRecord {
    pub number: u64,
    pub title: String,
    pub repo: String,
    pub url: String,
}

fn links(value: &Value, key: &str) -> Vec<LinkRecord> {
    list(value, key)
        .iter()
        .filter(|link| count(link, "number") > 0)
        .map(|link| LinkRecord {
            number: count(link, "number"),
            title: text(link, "title").to_owned(),
            repo: text(&link["repository"], "nameWithOwner").to_owned(),
            url: text(link, "url").to_owned(),
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckRow {
    pub name: String,
    pub workflow: String,
    pub state: CiState,
    /// The forge's word, lower case (`timed out`), for the row's state column.
    pub word: String,
    pub duration: Option<u64>,
    pub run: Option<u64>,
    pub job: Option<u64>,
    pub url: String,
    pub required: bool,
}

impl CheckRow {
    fn from(check: &Value, required: &[String]) -> Self {
        let name = check["name"]
            .as_str()
            .or_else(|| check["context"].as_str())
            .unwrap_or("check")
            .to_owned();
        let url = check["detailsUrl"]
            .as_str()
            .or_else(|| check["targetUrl"].as_str())
            .unwrap_or("")
            .to_owned();
        let id_after = |marker: &str| {
            url.split_once(marker).and_then(|(_, rest)| {
                rest.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|digits| digits.parse().ok())
            })
        };
        let word = check_word(check);
        let duration = match (
            epoch(text(check, "startedAt")),
            epoch(text(check, "completedAt")),
        ) {
            (Some(start), Some(end)) if end >= start => Some((end - start) as u64),
            _ => None,
        };
        Self {
            required: required.contains(&name),
            name,
            workflow: text(check, "workflowName").to_owned(),
            state: ci_state(word),
            word: word.to_ascii_lowercase().replace('_', " "),
            duration,
            // An Actions run only: an app's check (CodeQL) links GitHub's own check page,
            // `…/runs/<check id>`, which is no run.
            run: check["dustyRun"]
                .as_u64()
                .or_else(|| id_after("/actions/runs/")),
            job: id_after("/job/"),
            url,
        }
    }
}

/// `3s`, `2m 4s`, `1h 3m`.
pub fn duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRow {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// `A`, `M`, `D`, `R`.
    pub status: char,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRow {
    pub sha: String,
    pub headline: String,
    pub author: String,
    pub committed: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRecord {
    pub id: String,
    pub author: String,
    pub state: ReviewState,
    pub body: String,
    pub submitted: String,
}

fn review_state(word: &str) -> ReviewState {
    match word.to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        _ => ReviewState::Commented,
    }
}

/// The reader's pull request or issue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailRecord {
    pub row: ItemRecord,
    pub id: String,
    pub body: String,
    pub created: String,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    pub projects: Vec<String>,
    /// Issues: the pull requests that close it. Pull requests: the issues it closes.
    pub links: Vec<LinkRecord>,
    pub base: String,
    pub head: String,
    pub head_sha: String,
    pub mergeable: String,
    pub merge_state: String,
    pub reviews: Vec<ReviewRecord>,
    pub requested: Vec<String>,
    pub checks: Vec<CheckRow>,
    pub files: Vec<FileRow>,
    pub commits: Vec<CommitRow>,
    /// Reactions on the body, `(content, count)` in GitHub's words.
    pub reactions: Vec<(String, u64)>,
}

impl DetailRecord {
    pub fn from_value(value: &Value) -> Self {
        let mut row = ItemRecord::from_row(value);
        let extension = &value["dusty"];
        if row.updated.is_empty() {
            row.updated = text(extension, "updatedAt").to_owned();
        }
        if let Some(comments) = extension["comments"].as_u64() {
            row.comments = comments;
        }
        let required = list(extension, "required")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let statuses = list(extension, "files");
        let files = list(value, "files")
            .iter()
            .map(|file| {
                let path = text(file, "path").to_owned();
                let status = statuses
                    .iter()
                    .find(|entry| text(entry, "path") == path)
                    .map_or('M', |entry| match text(entry, "status") {
                        "Added" => 'A',
                        "Deleted" => 'D',
                        "Renamed" => 'R',
                        "Copied" => 'C',
                        _ => 'M',
                    });
                FileRow {
                    path,
                    additions: count(file, "additions"),
                    deletions: count(file, "deletions"),
                    status,
                }
            })
            .collect::<Vec<_>>();
        if row.additions.is_none() && !files.is_empty() {
            row.additions = Some(files.iter().map(|file| file.additions).sum());
            row.deletions = Some(files.iter().map(|file| file.deletions).sum());
        }
        let links = if value.get("closingIssuesReferences").is_some() {
            links(value, "closingIssuesReferences")
        } else {
            links(value, "closedByPullRequestsReferences")
        };
        Self {
            reactions: reactions(value),
            id: text(value, "id").to_owned(),
            body: text(value, "body").to_owned(),
            created: text(value, "createdAt").to_owned(),
            assignees: list(value, "assignees")
                .iter()
                .map(|user| text(user, "login").to_owned())
                .filter(|login| !login.is_empty())
                .collect(),
            milestone: value["milestone"]["title"]
                .as_str()
                .filter(|title| !title.is_empty())
                .map(str::to_owned),
            projects: list(value, "projectItems")
                .iter()
                .filter_map(|item| {
                    item["title"]
                        .as_str()
                        .or_else(|| item["project"]["title"].as_str())
                })
                .map(str::to_owned)
                .collect(),
            links,
            base: text(value, "baseRefName").to_owned(),
            head: text(value, "headRefName").to_owned(),
            head_sha: text(value, "headRefOid").to_owned(),
            mergeable: text(value, "mergeable").to_owned(),
            merge_state: text(value, "mergeStateStatus").to_owned(),
            reviews: list(value, "reviews")
                .iter()
                .map(|review| ReviewRecord {
                    id: text(review, "id").to_owned(),
                    author: login(review),
                    state: review_state(text(review, "state")),
                    body: text(review, "body").to_owned(),
                    submitted: text(review, "submittedAt").to_owned(),
                })
                .collect(),
            requested: list(extension, "reviewRequests")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            checks: list(value, "statusCheckRollup")
                .iter()
                .map(|check| CheckRow::from(check, &required))
                .collect(),
            files,
            commits: list(value, "commits")
                .iter()
                .map(|commit| CommitRow {
                    sha: text(commit, "oid").to_owned(),
                    headline: text(commit, "messageHeadline").to_owned(),
                    author: list(commit, "authors")
                        .first()
                        .map(|author| {
                            author["login"]
                                .as_str()
                                .or_else(|| author["name"].as_str())
                                .unwrap_or("")
                                .to_owned()
                        })
                        .unwrap_or_default(),
                    committed: text(commit, "committedDate").to_owned(),
                })
                .collect(),
            row,
        }
    }

    /// Each reviewer once: their latest submitted state, then the outstanding requests.
    pub fn reviewers(&self) -> Vec<(String, ReviewState)> {
        let mut slots: Vec<(String, ReviewState)> = Vec::new();
        for review in &self.reviews {
            if review.author == self.row.author {
                continue;
            }
            let state = review.state;
            match slots.iter_mut().find(|(login, _)| *login == review.author) {
                // A later comment does not undo an approval or a change request.
                Some(slot) if state != ReviewState::Commented => slot.1 = state,
                Some(_) => {}
                None => slots.push((review.author.clone(), state)),
            }
        }
        for login in &self.requested {
            match slots.iter_mut().find(|(name, _)| name == login) {
                Some(slot) => slot.1 = ReviewState::Requested,
                None => slots.push((login.clone(), ReviewState::Requested)),
            }
        }
        slots
    }

    pub fn check_counts(&self) -> CheckCounts {
        let mut counts = CheckCounts::default();
        for check in &self.checks {
            counts.add(check.state);
        }
        counts
    }
}

// ---------------------------------------------------------------------------------- timeline

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentRecord {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created: String,
    /// An optimistic comment the forge has not confirmed yet.
    pub pending: bool,
    /// The author's relation to the repository (`OWNER`, `MEMBER`, `CONTRIBUTOR`…), where the
    /// forge says.
    pub association: String,
    /// The body was edited after it was posted.
    pub edited: bool,
    /// `(content, count)` in GitHub's words (`+1`, `heart`…), where the forge sends them.
    pub reactions: Vec<(String, u64)>,
    /// The comment's own page, where the forge says.
    pub url: String,
}

/// GitHub's REST reaction rollup (`{"+1": 2, "heart": 1, "total_count": 3}`) or GraphQL
/// `reactionGroups`, as `(content, count)` pairs with a count.
pub fn reactions(value: &Value) -> Vec<(String, u64)> {
    const KINDS: [&str; 8] = [
        "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
    ];
    if let Some(groups) = value["reactionGroups"].as_array() {
        return groups
            .iter()
            .filter_map(|group| {
                let content = text(group, "content").to_ascii_lowercase();
                let content = match content.as_str() {
                    "thumbs_up" => "+1".to_owned(),
                    "thumbs_down" => "-1".to_owned(),
                    _ => content,
                };
                let count = group["users"]["totalCount"]
                    .as_u64()
                    .or_else(|| group["count"].as_u64())?;
                (count > 0).then_some((content, count))
            })
            .collect();
    }
    let rollup = &value["reactions"];
    KINDS
        .iter()
        .filter_map(|kind| {
            rollup[*kind]
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| ((*kind).to_owned(), n))
        })
        .collect()
}

/// A REST comment's (`author_association`, `updated_at` after `created_at`) or a GraphQL
/// node's (`authorAssociation`, `lastEditedAt`) association and edited mark.
fn comment_extras(value: &Value) -> (String, bool) {
    let association = value["author_association"]
        .as_str()
        .or_else(|| value["authorAssociation"].as_str())
        .unwrap_or("")
        .to_owned();
    let created = value["created_at"]
        .as_str()
        .or_else(|| value["createdAt"].as_str())
        .unwrap_or("");
    let edited = value["lastEditedAt"]
        .as_str()
        .is_some_and(|at| !at.is_empty())
        || value["updated_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty() && !created.is_empty() && at > created);
    (association, edited)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimelineEntry {
    Comment(CommentRecord),
    Review {
        review: ReviewRecord,
    },
    /// `labeled bug`, `closed this`, `merged commit 45be91c`.
    Event {
        actor: String,
        verb: String,
        /// The event's object in its own colour: a label, a login, a commit.
        object: Option<String>,
        label: Option<LabelChip>,
        link: Option<(LinkRecord, bool)>,
        at: String,
        kind: String,
    },
}

impl TimelineEntry {
    pub fn at(&self) -> &str {
        match self {
            Self::Comment(comment) => &comment.created,
            Self::Review { review } => &review.submitted,
            Self::Event { at, .. } => at,
        }
    }

    pub fn conversation(&self) -> bool {
        matches!(self, Self::Comment(_) | Self::Review { .. })
    }
}

/// Every event of the cached timeline pages (`[[event…]…]`), oldest first.
pub fn timeline(value: &Value) -> Vec<TimelineEntry> {
    let pages = value.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut entries = Vec::new();
    for event in pages
        .iter()
        .flat_map(|page| page.as_array().into_iter().flatten())
    {
        let kind = text(event, "event");
        let at = text(event, "created_at").to_owned();
        let actor = login(event);
        let entry = match kind {
            "commented" => {
                let (association, edited) = comment_extras(event);
                TimelineEntry::Comment(CommentRecord {
                    id: text(event, "node_id").to_owned(),
                    author: actor,
                    body: text(event, "body").to_owned(),
                    created: at,
                    pending: event["dustyPending"].as_bool() == Some(true),
                    association,
                    edited,
                    reactions: reactions(event),
                    url: text(event, "html_url").to_owned(),
                })
            }
            "reviewed" => {
                let state = review_state(text(event, "state"));
                let body = text(event, "body");
                // An empty "commented" review only carries inline threads.
                if state == ReviewState::Commented && body.trim().is_empty() {
                    continue;
                }
                TimelineEntry::Review {
                    review: ReviewRecord {
                        id: text(event, "node_id").to_owned(),
                        author: actor,
                        state,
                        body: body.to_owned(),
                        submitted: at,
                    },
                }
            }
            _ => {
                let mut object = None;
                let mut label = None;
                let mut link = None;
                let verb = match kind {
                    "labeled" | "unlabeled" => {
                        label = Some(LabelChip {
                            name: text(&event["label"], "name").to_owned(),
                            rgb: event["label"]["color"].as_str().and_then(hex),
                        });
                        (if kind == "labeled" {
                            "added"
                        } else {
                            "removed"
                        })
                        .into()
                    }
                    "assigned" => {
                        object = Some(text(&event["assignee"], "login").to_owned());
                        "assigned".into()
                    }
                    "unassigned" => {
                        object = Some(text(&event["assignee"], "login").to_owned());
                        "unassigned".into()
                    }
                    // GitHub's wording: "closed this as not planned", "closed
                    // this in 1a2b3c4" when a commit closed it.
                    "closed" => {
                        let sha = text(event, "commit_id");
                        if !sha.is_empty() {
                            object = Some(sha.chars().take(7).collect());
                            "closed this in".into()
                        } else {
                            match text(event, "state_reason") {
                                "completed" => "closed this as completed",
                                "not_planned" => "closed this as not planned",
                                "duplicate" => "closed this as a duplicate",
                                _ => "closed this",
                            }
                            .into()
                        }
                    }
                    "reopened" => "reopened this".into(),
                    "merged" => {
                        let sha = text(event, "commit_id");
                        object = (!sha.is_empty()).then(|| sha.chars().take(7).collect());
                        "merged commit".into()
                    }
                    "committed" => {
                        let sha = text(event, "sha");
                        object = Some(sha.chars().take(7).collect());
                        "pushed".into()
                    }
                    "head_ref_force_pushed" => {
                        let before = text(event, "before").chars().take(7).collect::<String>();
                        let after = text(event, "after").chars().take(7).collect::<String>();
                        // "force-pushed the fix/backoff branch from 1a2b3c4 to 5d6e7f8"; GitHub's
                        // REST timeline names only the new head (`commit_id`).
                        object = Some(if before.is_empty() {
                            format!("to {after}")
                        } else {
                            format!("from {before} to {after}")
                        });
                        "force-pushed".into()
                    }
                    "cross-referenced" => {
                        let issue = &event["source"]["issue"];
                        if count(issue, "number") > 0 {
                            link = Some((
                                LinkRecord {
                                    number: count(issue, "number"),
                                    title: text(issue, "title").to_owned(),
                                    repo: text(&issue["repository"], "full_name").to_owned(),
                                    url: text(issue, "html_url").to_owned(),
                                },
                                issue.get("pull_request").is_some(),
                            ));
                        }
                        "mentioned this in".into()
                    }
                    "renamed" => {
                        object = Some(format!(
                            "{} → {}",
                            text(&event["rename"], "from"),
                            text(&event["rename"], "to")
                        ));
                        "changed the title".into()
                    }
                    "review_requested" | "review_request_removed" => {
                        let reviewer = &event["requested_reviewer"];
                        let team = text(&event["requested_team"], "name");
                        object = Some(
                            if reviewer.is_object() {
                                text(reviewer, "login")
                            } else {
                                team
                            }
                            .to_owned(),
                        );
                        (if kind == "review_requested" {
                            "requested review from"
                        } else {
                            "removed the request for review from"
                        })
                        .into()
                    }
                    "review_dismissed" | "dismiss review" => "dismissed a review".into(),
                    "ready_for_review" => "marked this pull request as ready for review".into(),
                    "convert_to_draft" => "marked this pull request as draft".into(),
                    // "locked as off-topic and limited conversation to collaborators".
                    "locked" | "lock" => match text(event, "lock_reason") {
                        "" => "locked and limited conversation to collaborators".into(),
                        reason => format!(
                            "locked as {} and limited conversation to collaborators",
                            reason.replace('_', " ")
                        ),
                    },
                    "unlocked" | "unlock" => "unlocked this conversation".into(),
                    "pinned" | "pin" => "pinned this issue".into(),
                    "unpinned" | "unpin" => "unpinned this issue".into(),
                    "base_ref_changed" | "change target branch" => {
                        let (from, to) = (
                            text(event, "previous_ref_name"),
                            text(event, "current_ref_name"),
                        );
                        if !from.is_empty() && !to.is_empty() {
                            object = Some(format!("from {from} to {to}"));
                        }
                        "changed the base branch".into()
                    }
                    "deployed" => {
                        let environment = text(&event["deployment"], "environment");
                        if environment.is_empty() {
                            "deployed this".into()
                        } else {
                            format!("deployed to {environment}")
                        }
                    }
                    "marked_as_duplicate" => "marked this as a duplicate".into(),
                    "unmarked_as_duplicate" => "marked this as not a duplicate".into(),
                    "transferred" => "transferred this issue".into(),
                    "converted_to_discussion" => "converted this issue into a discussion".into(),
                    "auto_merge_enabled" => "enabled auto-merge".into(),
                    "auto_merge_disabled" => "disabled auto-merge".into(),
                    "comment_deleted" => "deleted a comment".into(),
                    "added_to_project" | "added_to_project_v2" => "added this to a project".into(),
                    "removed_from_project" | "removed_from_project_v2" => {
                        "removed this from a project".into()
                    }
                    "moved_columns_in_project" => {
                        let column = text(&event["project_card"], "column_name");
                        object = (!column.is_empty()).then(|| column.to_owned());
                        "moved this to".into()
                    }
                    // Kept in the record for the page to word: the target and the branch
                    // names are the page's own (issue::events_row).
                    "referenced" => {
                        let sha = text(event, "commit_id");
                        object = (!sha.is_empty()).then(|| sha.chars().take(7).collect());
                        "referenced".into()
                    }
                    "head_ref_deleted" | "delete branch" => "deleted the head branch".into(),
                    "head_ref_restored" => "restored the head branch".into(),
                    // REST lists these; GitHub's page never shows them.
                    "mentioned" | "subscribed" | "unsubscribed" | "user_blocked" => continue,
                    // GitHub's wording: "added this to the v1.4 milestone".
                    "milestoned" | "demilestoned" => {
                        let title = text(&event["milestone"], "title");
                        object = (!title.is_empty()).then(|| format!("{title} milestone"));
                        (if kind == "milestoned" {
                            "added this to the"
                        } else {
                            "removed this from the"
                        })
                        .into()
                    }
                    // The page words these with their target (issue::events_row).
                    "connected" => "linked".into(),
                    "disconnected" => "removed a link".into(),
                    "" => continue,
                    other => other.replace('_', " "),
                };
                TimelineEntry::Event {
                    actor,
                    verb,
                    object,
                    label,
                    link,
                    at,
                    kind: kind.to_owned(),
                }
            }
        };
        entries.push(entry);
    }
    entries
}

// ---------------------------------------------------------------------------------- threads

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadCard {
    pub id: String,
    pub resolved: Option<bool>,
    pub path: String,
    pub line: Option<u64>,
    /// The thread sits on a line that is no longer in the diff.
    pub outdated: bool,
    /// `LEFT` or `RIGHT`.
    pub side: String,
    pub commit: String,
    pub comments: Vec<CommentRecord>,
    pub total: u64,
    /// The diff lines the thread hangs on (`@@ … @@` and the lines up to its line).
    pub hunk: String,
    /// The first line of a multi-line thread (GitHub's `startLine`).
    pub start_line: Option<u64>,
}

impl ThreadCard {
    pub fn author(&self) -> &str {
        self.comments
            .first()
            .map_or("", |comment| comment.author.as_str())
    }

    pub fn replies(&self) -> u64 {
        self.total.max(self.comments.len() as u64).saturating_sub(1)
    }
}

/// Review threads from the cached GraphQL pages.
pub fn threads(value: &Value) -> Vec<ThreadCard> {
    let pages = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    pages
        .iter()
        .flat_map(|page| {
            list(
                &page["data"]["repository"]["pullRequest"]["reviewThreads"],
                "nodes",
            )
        })
        .map(|thread| {
            let nodes = list(&thread["comments"], "nodes");
            let first = nodes.first().cloned().unwrap_or(Value::Null);
            let path = thread["path"]
                .as_str()
                .filter(|path| !path.is_empty())
                .unwrap_or(text(&first, "path"))
                .to_owned();
            let current = thread["line"].as_u64().or_else(|| first["line"].as_u64());
            let original = thread["originalLine"]
                .as_u64()
                .or_else(|| first["originalLine"].as_u64());
            ThreadCard {
                id: text(thread, "id").to_owned(),
                resolved: thread["isResolved"].as_bool(),
                path,
                line: current.or(original),
                outdated: current.is_none() && original.is_some(),
                side: thread["diffSide"].as_str().unwrap_or("RIGHT").to_owned(),
                commit: text(&first["commit"], "oid").to_owned(),
                comments: nodes
                    .iter()
                    .map(|comment| {
                        let (association, edited) = comment_extras(comment);
                        CommentRecord {
                            id: text(comment, "id").to_owned(),
                            author: login(comment),
                            body: text(comment, "body").to_owned(),
                            created: text(comment, "createdAt").to_owned(),
                            pending: false,
                            association,
                            edited,
                            reactions: reactions(comment),
                            url: text(comment, "url").to_owned(),
                        }
                    })
                    .collect(),
                total: thread["comments"]["totalCount"]
                    .as_u64()
                    .unwrap_or(nodes.len() as u64),
                hunk: text(&first, "diffHunk").to_owned(),
                start_line: thread["startLine"].as_u64(),
            }
        })
        .collect()
}

/// `path → viewed` from the cached viewed-state pages.
pub fn viewed(value: &Value, path: &str) -> Option<bool> {
    let pages = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    pages.iter().find_map(|page| {
        list(&page["data"]["repository"]["pullRequest"]["files"], "nodes")
            .iter()
            .find(|file| text(file, "path") == path)
            .map(|file| text(file, "viewerViewedState") == "VIEWED")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stamps_become_relative_ages_and_dates() {
        let now = epoch("2026-11-14T12:00:00Z").unwrap();
        assert_eq!(ago("2026-11-14T11:59:30Z", now), "just now");
        assert_eq!(ago("2026-11-14T11:42:00Z", now), "18m ago");
        assert_eq!(ago("2026-11-14T09:00:00+00:00", now), "3h ago");
        assert_eq!(ago("2026-11-14T10:00:00+01:00", now), "3h ago");
        assert_eq!(ago("2026-11-12T12:00:00Z", now), "2d ago");
        assert_eq!(ago("2026-10-03T12:00:00Z", now), "Oct 3");
        assert_eq!(ago("2024-10-03", now), "Oct 3 2024");
        assert_eq!(ago("", now), "");
        assert_eq!(ago("soon", now), "");
        assert_eq!(epoch("1970-01-02"), Some(86_400));
        assert_eq!(
            epoch("2026-11-08T10:00:00.123Z"),
            epoch("2026-11-08T10:00:00Z")
        );
    }

    #[test]
    fn list_rows_keep_lifecycle_review_ci_and_label_colours() {
        let row = json!({
            "number": 13, "title": "Clamp backoff", "state": "OPEN", "isDraft": false,
            "author": {"login": "maya"}, "updatedAt": "2026-11-14T11:00:00Z",
            "labels": [{"name": "priority: high", "color": "fbca04"}, {"name": "plain"}],
            "comments": [{}, {}],
            "reviewDecision": "REVIEW_REQUIRED",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}, {"conclusion": "FAILURE"},
                                  {"status": "IN_PROGRESS"}],
            "dusty": {"additions": 6, "deletions": 2},
        });
        let record = ItemRecord::from_row(&row);
        assert_eq!(record.lifecycle, Lifecycle::Open);
        assert_eq!(record.review, Some(ReviewState::Requested));
        assert_eq!(record.ci(), Some(CiState::Failed));
        assert_eq!(record.checks.summary(), "1 failed · 1 pending · 1 passed");
        assert_eq!((record.additions, record.deletions), (Some(6), Some(2)));
        assert_eq!(record.comments, 2);
        assert_eq!(record.labels[0].rgb, Some([0xfb, 0xca, 0x04]));
        assert_eq!(record.labels[1].rgb, None);
        assert!(!record.gone);
    }

    #[test]
    fn a_row_counts_approvals_against_open_requests_like_review_decision() {
        let row = |decision: &str, reviews: Value| {
            ItemRecord::from_row(
                &json!({"number": 13, "reviewDecision": decision, "dusty": {"reviews": reviews}}),
            )
        };
        let fact = |record: ItemRecord| {
            record
                .review_fact(None)
                .map(|(state, words)| (state, words.unwrap_or_default()))
        };
        // alex approved, sam is still asked: not "Review requested".
        assert_eq!(
            fact(row(
                "REVIEW_REQUIRED",
                json!({"approved": 1, "changesRequested": 0, "pending": 1})
            )),
            Some((ReviewState::Approved, "1 approval · 1 pending".into()))
        );
        assert_eq!(
            fact(row(
                "REVIEW_REQUIRED",
                json!({"approved": 2, "changesRequested": 0, "pending": 0})
            )),
            Some((ReviewState::Approved, "2 approvals".into()))
        );
        // A change request outweighs approvals; nothing yet stays the decision's word.
        assert_eq!(
            fact(row(
                "REVIEW_REQUIRED",
                json!({"approved": 1, "changesRequested": 1, "pending": 0})
            )),
            Some((ReviewState::ChangesRequested, String::new()))
        );
        // No review required and none asked: no fact, as before.
        assert_eq!(
            fact(row(
                "",
                json!({"approved": 1, "changesRequested": 0, "pending": 0})
            )),
            None
        );
        assert_eq!(
            fact(row(
                "REVIEW_REQUIRED",
                json!({"approved": 0, "changesRequested": 0, "pending": 2})
            )),
            Some((ReviewState::Requested, String::new()))
        );
        assert_eq!(
            fact(row(
                "APPROVED",
                json!({"approved": 1, "changesRequested": 0, "pending": 0})
            )),
            Some((ReviewState::Approved, String::new()))
        );
        // A forge whose list knows only the requests (Gitea) counts from the opened item.
        let gitea =
            ItemRecord::from_row(&json!({"number": 1412, "dusty": {"reviewRequested": true}}));
        let opened = tally_of(&[
            ("alex".into(), ReviewState::Approved),
            ("sam".into(), ReviewState::Requested),
        ]);
        assert_eq!(
            gitea.review_fact(Some(opened)),
            Some((ReviewState::Approved, Some("1 approval · 1 pending".into())))
        );
        assert_eq!(
            gitea.review_fact(None),
            Some((ReviewState::Requested, None))
        );
    }

    #[test]
    fn closed_reasons_drafts_and_merges_are_distinct_lifecycles() {
        let state = |value: Value| ItemRecord::from_row(&value).lifecycle;
        assert_eq!(
            state(json!({"state": "OPEN", "isDraft": true})),
            Lifecycle::Draft
        );
        assert_eq!(state(json!({"state": "MERGED"})), Lifecycle::Merged);
        assert_eq!(state(json!({"state": "CLOSED"})), Lifecycle::Closed);
        assert_eq!(
            state(json!({"state": "CLOSED", "dusty": {"stateReason": "NOT_PLANNED"}})),
            Lifecycle::ClosedNotPlanned
        );
        assert_eq!(
            state(json!({"state": "CLOSED", "stateReason": "not_planned"})),
            Lifecycle::ClosedNotPlanned
        );
    }

    #[test]
    fn check_words_bucket_like_the_forge_rollup() {
        let mut counts = CheckCounts::default();
        for word in [
            "SUCCESS",
            "NEUTRAL",
            "TIMED_OUT",
            "QUEUED",
            "STALE",
            "",
            "weird",
        ] {
            counts.add(ci_state(word));
        }
        assert_eq!(
            counts,
            CheckCounts {
                passed: 1,
                failed: 2,
                pending: 1,
                skipped: 1,
                unknown: 2
            }
        );
        assert_eq!(counts.state(), Some(CiState::Failed));
        assert_eq!(CheckCounts::default().state(), None);
        assert_eq!(check_word(&json!({"state": "PENDING"})), "PENDING");
        assert_eq!(
            check_word(&json!({"conclusion": "", "status": "QUEUED"})),
            "QUEUED"
        );
    }

    #[test]
    fn details_carry_checks_with_run_links_files_and_reviewers() {
        let value = json!({
            "number": 9, "title": "Native review", "state": "OPEN", "author": {"login": "maya"},
            "baseRefName": "main", "headRefName": "fix/backoff", "headRefOid": "c92ad11aaaa",
            "files": [{"path": "orbit/queue.py", "additions": 4, "deletions": 1},
                      {"path": "tests/test_queue.py", "additions": 2, "deletions": 1}],
            "reviews": [
                {"author": {"login": "alex"}, "state": "APPROVED", "submittedAt": "2026-11-01"},
                {"author": {"login": "alex"}, "state": "COMMENTED", "submittedAt": "2026-11-02"},
                {"author": {"login": "maya"}, "state": "COMMENTED"},
            ],
            "statusCheckRollup": [{
                "name": "Python tests", "conclusion": "FAILURE", "workflowName": "Quality checks",
                "detailsUrl": "https://github.com/o/r/actions/runs/91/job/42",
                "startedAt": "2026-11-08T10:00:00Z", "completedAt": "2026-11-08T10:00:03Z",
            }],
            "closingIssuesReferences": [{"number": 1, "title": "Queue stalls"}],
            "dusty": {"reviewRequests": ["sam"], "required": ["Python tests"],
                      "files": [{"path": "tests/test_queue.py", "status": "Added"}]},
        });
        let detail = DetailRecord::from_value(&value);
        assert_eq!(
            (detail.row.additions, detail.row.deletions),
            (Some(6), Some(2))
        );
        assert_eq!(detail.files[1].status, 'A');
        assert_eq!(
            detail.reviewers(),
            vec![
                ("alex".to_owned(), ReviewState::Approved),
                ("sam".to_owned(), ReviewState::Requested)
            ]
        );
        let check = &detail.checks[0];
        assert_eq!(
            (check.run, check.job, check.duration),
            (Some(91), Some(42), Some(3))
        );
        assert!(check.required);
        assert_eq!(check.word, "failure");
        assert_eq!(detail.links[0].number, 1);
        assert_eq!(duration(184), "3m 4s");
    }

    #[test]
    fn a_check_names_a_run_only_from_an_actions_link() {
        let actions = json!({"name": "Python tests", "detailsUrl": "https://github.com/o/r/actions/runs/91/job/42"});
        let app =
            json!({"name": "CodeQL", "detailsUrl": "https://github.com/o/r/runs/133093884570"});
        let actions = CheckRow::from(&actions, &[]);
        assert_eq!((actions.run, actions.job), (Some(91), Some(42)));
        assert_eq!(CheckRow::from(&app, &[]).run, None);
    }

    #[test]
    fn timelines_split_conversation_from_events_and_drop_empty_reviews() {
        let value = json!([[
            {"event": "labeled", "actor": {"login": "alice"}, "label": {"name": "bug", "color": "d73a4a"},
             "created_at": "2026-11-08"},
            {"event": "commented", "user": {"login": "tester"}, "body": "Hi", "created_at": "2026-11-08",
             "node_id": "C1"},
            {"event": "reviewed", "user": {"login": "rev"}, "state": "commented", "body": ""},
            {"event": "cross-referenced", "actor": {"login": "sam"},
             "source": {"issue": {"number": 11, "title": "Recover", "pull_request": {}}}},
        ], [
            {"event": "commented", "user": {"login": "you"}, "body": "Sending", "dustyPending": true},
        ]]);
        let entries = timeline(&value);
        assert_eq!(entries.len(), 4);
        assert!(
            matches!(&entries[0], TimelineEntry::Event { label: Some(label), .. }
            if label.rgb == Some([0xd7, 0x3a, 0x4a]))
        );
        assert!(entries[1].conversation());
        assert!(
            matches!(&entries[2], TimelineEntry::Event { link: Some((link, true)), .. }
            if link.number == 11)
        );
        assert!(matches!(&entries[3], TimelineEntry::Comment(comment) if comment.pending));
    }

    /// GitHub's timeline phrasing, event by event.
    #[test]
    fn timeline_events_read_in_githubs_words() {
        let event = |kind: &str, extra: Value| {
            let mut value =
                json!({"event": kind, "actor": {"login": "maya"}, "created_at": "2026-11-08"});
            for (key, field) in extra.as_object().cloned().unwrap_or_default() {
                value[key] = field;
            }
            value
        };
        let cases = [
            (
                event(
                    "renamed",
                    json!({"rename": {"from": "Clamp backoff", "to": "Clamp exponential backoff"}}),
                ),
                "changed the title",
                Some("Clamp backoff → Clamp exponential backoff"),
            ),
            (
                event(
                    "base_ref_changed",
                    json!({"previous_ref_name": "main", "current_ref_name": "release"}),
                ),
                "changed the base branch",
                Some("from main to release"),
            ),
            (
                event("ready_for_review", json!({})),
                "marked this pull request as ready for review",
                None,
            ),
            (
                event("convert_to_draft", json!({})),
                "marked this pull request as draft",
                None,
            ),
            (
                event("locked", json!({"lock_reason": "off-topic"})),
                "locked as off-topic and limited conversation to collaborators",
                None,
            ),
            (
                event("locked", json!({"lock_reason": "too heated"})),
                "locked as too heated and limited conversation to collaborators",
                None,
            ),
            (
                event("locked", json!({})),
                "locked and limited conversation to collaborators",
                None,
            ),
            (
                event("unlocked", json!({})),
                "unlocked this conversation",
                None,
            ),
            (event("pinned", json!({})), "pinned this issue", None),
            (event("unpinned", json!({})), "unpinned this issue", None),
            (
                event(
                    "deployed",
                    json!({"deployment": {"environment": "production"}}),
                ),
                "deployed to production",
                None,
            ),
            (
                event("closed", json!({"state_reason": "not_planned"})),
                "closed this as not planned",
                None,
            ),
            (
                event("closed", json!({"state_reason": "completed"})),
                "closed this as completed",
                None,
            ),
            (
                event("closed", json!({"commit_id": "5d6e7f8a9b"})),
                "closed this in",
                Some("5d6e7f8"),
            ),
            (event("closed", json!({})), "closed this", None),
            (event("reopened", json!({})), "reopened this", None),
            (
                event("merged", json!({"commit_id": "1a2b3c4d5e"})),
                "merged commit",
                Some("1a2b3c4"),
            ),
            (
                event(
                    "head_ref_force_pushed",
                    json!({"before": "1a2b3c4d", "after": "5d6e7f8a"}),
                ),
                "force-pushed",
                Some("from 1a2b3c4 to 5d6e7f8"),
            ),
            (
                event(
                    "review_requested",
                    json!({"requested_reviewer": {"login": "alex"}}),
                ),
                "requested review from",
                Some("alex"),
            ),
            (
                event(
                    "review_request_removed",
                    json!({"requested_reviewer": {"login": "alex"}}),
                ),
                "removed the request for review from",
                Some("alex"),
            ),
            (
                event("milestoned", json!({"milestone": {"title": "v1.4"}})),
                "added this to the",
                Some("v1.4 milestone"),
            ),
            (
                event("demilestoned", json!({"milestone": {"title": "v1.4"}})),
                "removed this from the",
                Some("v1.4 milestone"),
            ),
            (
                event("marked_as_duplicate", json!({})),
                "marked this as a duplicate",
                None,
            ),
            (
                event("unmarked_as_duplicate", json!({})),
                "marked this as not a duplicate",
                None,
            ),
            (
                event("auto_merge_enabled", json!({})),
                "enabled auto-merge",
                None,
            ),
            (
                event("auto_merge_disabled", json!({})),
                "disabled auto-merge",
                None,
            ),
            (
                event(
                    "moved_columns_in_project",
                    json!({"project_card": {"column_name": "Done"}}),
                ),
                "moved this to",
                Some("Done"),
            ),
            (
                event("lock", json!({})),
                "locked and limited conversation to collaborators",
                None,
            ),
        ];
        for (value, verb, object) in cases {
            let entries = timeline(&json!([[value.clone()]]));
            let [
                TimelineEntry::Event {
                    verb: read,
                    object: read_object,
                    ..
                },
            ] = entries.as_slice()
            else {
                panic!("{value}: {entries:?}");
            };
            assert_eq!(
                (read.as_str(), read_object.as_deref()),
                (verb, object),
                "{value}"
            );
        }
        // REST lists these; GitHub's page never shows them.
        for kind in ["mentioned", "subscribed", "unsubscribed"] {
            assert!(
                timeline(&json!([[event(kind, json!({}))]])).is_empty(),
                "{kind}"
            );
        }
    }

    #[test]
    fn threads_read_anchor_state_and_reply_counts() {
        let value = json!([{"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [
            {"id": "T1", "isResolved": true, "comments": {"totalCount": 3, "nodes": [
                {"author": {"login": "alex"}, "body": "Cap it", "path": "search.py",
                 "line": null, "originalLine": 13, "commit": {"oid": "abc"}},
                {"author": {"login": "sam"}, "body": "Done"}]}}
        ]}}}}}]);
        let cards = threads(&value);
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(
            (card.path.as_str(), card.line, card.outdated),
            ("search.py", Some(13), true)
        );
        assert_eq!(card.author(), "alex");
        assert_eq!(card.replies(), 2);
        assert_eq!(card.resolved, Some(true));
    }
}
