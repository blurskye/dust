//! The normalised forge model (FORGE.md §3.3). Every provider maps its responses into these
//! types; the UI (through `legacy.rs` until DESIGN phase 4), the cache and `dusty-git forge`
//! read only these. Ids are strings, numbers are `u64` (GitLab `iid`).

use serde::Deserialize;
use serde::Serialize;
use std::time::Duration;

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ItemId(pub String);

impl ItemId {
    pub fn new(id: impl ToString) -> Self {
        Self(id.to_string())
    }
}

impl From<&str> for ItemId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for ItemId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<u64> for ItemId {
    fn from(id: u64) -> Self {
        Self(id.to_string())
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// RFC 3339 as the forge sent it; it sorts lexically within one forge.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Timestamp(pub String);

impl Timestamp {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
    /// Gitea's rebase-then-merge-commit.
    RebaseMerge,
    FastForwardOnly,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Permissions {
    pub admin: bool,
    pub push: bool,
    pub pull: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub id: ItemId,
    pub full_name: String,
    pub default_branch: String,
    pub web_url: String,
    pub is_fork: bool,
    pub parent: Option<String>,
    /// The parent's default branch, so a fork's PR form targets upstream (extension).
    pub parent_default_branch: Option<String>,
    pub permissions: Permissions,
    pub merge_methods: Vec<MergeMethod>,
    pub default_merge_method: Option<MergeMethod>,
    pub delete_branch_on_merge: bool,
    pub has_ci: bool,
    pub pr_template: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    pub id: Option<ItemId>,
    pub name: Option<String>,
}

impl User {
    pub fn login(login: impl Into<String>) -> Self {
        Self {
            login: login.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub id: Option<ItemId>,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub id: Option<ItemId>,
    pub title: String,
    pub due: Option<Timestamp>,
    pub open: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    PullRequest,
    Issue,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Lifecycle {
    #[default]
    Open,
    Draft,
    Merged,
    Closed,
    ClosedNotPlanned,
}

impl Lifecycle {
    pub fn is_open(self) -> bool {
        matches!(self, Lifecycle::Open | Lifecycle::Draft)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ReviewState {
    NotRequested,
    Requested,
    Approved,
    ChangesRequested,
    Commented,
    /// GitHub keeps dismissed reviews in the history (extension).
    Dismissed,
    /// A review the viewer has started but not submitted (extension).
    Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum CiState {
    Passed,
    Failed,
    Pending,
    Skipped,
    Unknown,
    Cancelled,
    ActionRequired,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ItemRow {
    pub kind: Option<ItemKind>,
    pub number: u64,
    pub id: ItemId,
    pub title: String,
    pub author: User,
    pub lifecycle: Lifecycle,
    pub labels: Vec<Label>,
    pub updated: Timestamp,
    pub comments: u64,
    /// `owner/name`; set for cross-repo rows (inbox).
    pub repo: Option<String>,
    pub web_url: String,
    pub review: Option<ReviewState>,
    pub ci: Option<CiState>,
    /// Check counts behind `ci`, for the row's `×1 …2 ✓5` slot (extension).
    pub ci_counts: Vec<(CiState, u64)>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    pub draft: bool,
    /// At most 2 KB of the description, enough for the list's reading pane (§7.5).
    pub body_preview: Option<String>,
    /// A pull request's source branch, when the list query carries it (the IDE picker, F7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefInfo {
    pub repo: Option<String>,
    pub branch: String,
    pub sha: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Mergeable {
    Clean,
    Behind,
    Blocked,
    Conflicting,
    Draft,
    HasHooks,
    Unstable,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReviewerSlot {
    pub reviewer: String,
    pub state: ReviewState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: ItemId,
    pub author: User,
    pub state: ReviewState,
    pub body: String,
    pub submitted: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunLink {
    pub run: ItemId,
    pub job: Option<ItemId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub workflow: Option<String>,
    pub state: CiState,
    /// The forge's own word (`SUCCESS`, `TIMED_OUT`, `IN_PROGRESS`…), kept for display (extension).
    pub raw_state: Option<String>,
    pub started: Option<Timestamp>,
    #[serde(skip)]
    pub duration: Option<Duration>,
    pub completed: Option<Timestamp>,
    pub details_url: Option<String>,
    pub run_link: Option<RunLink>,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Changed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u64,
    pub deletions: u64,
    pub viewed: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitSummary {
    pub sha: String,
    pub headline: String,
    pub body: String,
    pub committed: Timestamp,
    pub author: Option<User>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkRef {
    pub number: u64,
    pub title: String,
    pub repo: Option<String>,
    pub web_url: Option<String>,
    pub kind: Option<ItemKind>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub state: String,
    pub mergeable: String,
    pub head: String,
    pub rollup: Option<CiState>,
    pub updated: Timestamp,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub row: ItemRow,
    pub body: String,
    pub base: RefInfo,
    pub head: RefInfo,
    pub mergeable: Mergeable,
    pub reviewers: Vec<ReviewerSlot>,
    pub reviews: Vec<Review>,
    pub checks: Vec<Check>,
    pub files: Vec<ChangedFile>,
    pub commits: Vec<CommitSummary>,
    pub assignees: Vec<User>,
    pub milestone: Option<Milestone>,
    pub linked_issues: Vec<LinkRef>,
    pub created: Timestamp,
    pub pinned_head: String,
    pub fingerprint: Fingerprint,
    /// GitHub Projects the item belongs to (kept where already shown, §4.2).
    pub projects: Vec<String>,
    pub subscribed: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ClosedReason {
    Completed,
    NotPlanned,
    Duplicate,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub row: ItemRow,
    pub body: String,
    pub assignees: Vec<User>,
    pub milestone: Option<Milestone>,
    pub linked_prs: Vec<LinkRef>,
    pub created: Timestamp,
    pub closed_reason: Option<ClosedReason>,
    pub projects: Vec<String>,
    pub subscribed: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Side {
    Left,
    #[default]
    Right,
}

impl Side {
    pub fn as_github(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

/// Where a comment sits in the diff; GitHub keeps it per comment (extension).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommentPosition {
    pub path: String,
    pub line: Option<u64>,
    pub original_line: Option<u64>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: ItemId,
    pub author: User,
    pub body: String,
    pub created: Timestamp,
    pub system: bool,
    pub pending: bool,
    pub position: Option<CommentPosition>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReviewThread {
    pub id: ItemId,
    /// `None` when the forge cannot say (GitHub omits it for some legacy threads).
    pub resolved: Option<bool>,
    pub resolvable: bool,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub original_line: Option<u64>,
    pub side: Side,
    pub commit: Option<String>,
    pub comments: Vec<Comment>,
    /// The thread's comment count when more than the loaded page exist (extension).
    pub total_comments: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Actor {
    pub actor: User,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TimelineEvent {
    Commented(Comment),
    Reviewed(Review),
    Labeled { label: String, by: Actor },
    Unlabeled { label: String, by: Actor },
    Assigned { assignee: String, by: Actor },
    Unassigned { assignee: String, by: Actor },
    Closed { by: Actor },
    Reopened { by: Actor },
    Merged { sha: String, by: Actor },
    Committed(CommitSummary),
    ForcePushed { from: String, to: String, by: Actor },
    CrossReferenced { link: LinkRef, by: Actor },
    Renamed { from: String, to: String, by: Actor },
    ReviewRequested { reviewer: String, by: Actor },
    Other { kind: String, by: Actor },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: ItemId,
    pub name: String,
    pub path: String,
    pub state: String,
    pub dispatchable: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum RunStatus {
    #[default]
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: ItemId,
    pub number: u64,
    pub title: String,
    pub workflow: Option<Workflow>,
    pub status: RunStatus,
    pub conclusion: Option<CiState>,
    /// The forge's own conclusion word (`success`, `neutral`, `timed_out`…), kept for display.
    pub conclusion_raw: Option<String>,
    pub branch: String,
    pub sha: String,
    pub event: String,
    pub created: Timestamp,
    #[serde(skip)]
    pub duration: Option<Duration>,
    pub attempt: u32,
    pub attempts: u32,
    pub web_url: String,
    pub pr_number: Option<u64>,
    pub needs_approval: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub number: u64,
    pub name: String,
    pub status: RunStatus,
    pub conclusion: Option<CiState>,
    pub conclusion_raw: Option<String>,
    pub started: Option<Timestamp>,
    pub completed: Option<Timestamp>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Steps {
    Known(Vec<Step>),
    FromLogSections,
    Unavailable(String),
}

impl Default for Steps {
    fn default() -> Self {
        Steps::Known(Vec::new())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: ItemId,
    pub name: String,
    pub status: RunStatus,
    pub conclusion: Option<CiState>,
    pub conclusion_raw: Option<String>,
    pub started: Option<Timestamp>,
    pub completed: Option<Timestamp>,
    pub steps: Steps,
    pub web_url: String,
    pub stage: Option<String>,
    pub retried: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FailureLine {
    pub job: ItemId,
    pub line: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunDetail {
    pub run: Run,
    pub jobs: Vec<Job>,
    pub failure_summary: Option<FailureLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LogFormat {
    GhTabbed,
    GitLabSections,
    Plain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Log {
    pub job: ItemId,
    /// Raw, ANSI included.
    pub text: String,
    pub format: LogFormat,
    pub truncated: bool,
    /// The log's own sections (GitLab `section_start`, GitHub and Gitea `##[group]`), in
    /// order; `logs::split` refines them with a job's known steps.
    #[serde(default)]
    pub sections: Vec<LogSection>,
}

impl Log {
    /// A log with its sections parsed from the text.
    pub fn new(job: ItemId, text: String, format: LogFormat, truncated: bool) -> Self {
        let sections = if truncated {
            Vec::new()
        } else {
            logs::sections(&text, format)
        };
        Self {
            job,
            text,
            format,
            truncated,
            sections,
        }
    }
}

/// A titled run of log lines: a GitLab trace section, a `##[group]`, or a step's share of the
/// log. `start..end` are line indexes into `Log::text` (end exclusive). ANSI stays in the text.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogSection {
    pub name: String,
    /// The step this section is, when the job's steps are known.
    pub step: Option<u64>,
    pub start: usize,
    pub end: usize,
    /// Nesting depth (GitLab sections nest; 0 is top level).
    pub depth: u8,
    pub started: Option<Timestamp>,
    pub completed: Option<Timestamp>,
    /// GitLab's `[collapsed=true]` option.
    pub collapsed: bool,
}

/// One file inside an artifact archive.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactFile {
    pub path: String,
    pub size: u64,
    pub compressed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArtifactSource {
    Url(String),
    Cli,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ItemId,
    pub name: String,
    pub size: u64,
    pub expires: Option<Timestamp>,
    pub download: ArtifactSource,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NotifyReason {
    Mentioned,
    ReviewRequested,
    Assigned,
    Author,
    Comment,
    StateChange,
    CiActivity,
    Subscribed,
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NotifyTarget {
    Item { kind: ItemKind, number: u64 },
    Run(ItemId),
    Release(String),
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: ItemId,
    pub title: String,
    pub reason: NotifyReason,
    pub unread: bool,
    pub repo: String,
    pub target: NotifyTarget,
    pub updated: Timestamp,
    pub comment: Option<ItemId>,
}

pub mod logs {
    //! Job log sections and step slicing (F6). GitLab traces carry
    //! `section_start:<unix>:<name>[options]` / `section_end:<unix>:<name>` markers; GitHub and
    //! Gitea logs carry `##[group]Title` / `##[endgroup]` and an RFC 3339 timestamp per line;
    //! `gh run view --log` lines are `job<TAB>step<TAB>line`.
    use super::Log;
    use super::LogFormat;
    use super::LogSection;
    use super::Step;
    use super::Timestamp;

    /// The sections a log names itself.
    pub fn sections(text: &str, format: LogFormat) -> Vec<LogSection> {
        match format {
            LogFormat::GitLabSections => gitlab(text),
            LogFormat::GhTabbed => tabbed(text),
            LogFormat::Plain => groups(text),
        }
    }

    /// The log cut by the job's known steps: each line goes to the step running at its
    /// timestamp, a `##[group]` naming a later step moves on to it. Without steps (or without
    /// timestamps to place lines) the log's own sections are returned.
    pub fn split(log: &Log, steps: &[Step]) -> Vec<LogSection> {
        if steps.is_empty() || log.format == LogFormat::GitLabSections {
            return if log.sections.is_empty() {
                sections(&log.text, log.format)
            } else {
                log.sections.clone()
            };
        }
        let mut ordered = steps.to_vec();
        ordered.sort_by_key(|step| step.number);
        let seconds = |time: &Option<Timestamp>| {
            time.as_ref()
                .and_then(|time| crate::forge::cache::unix_seconds(time.as_str()))
        };
        let lines = log.text.lines().collect::<Vec<_>>();
        if !lines.iter().any(|line| line_time(line).is_some()) {
            return if log.sections.is_empty() {
                sections(&log.text, log.format)
            } else {
                log.sections.clone()
            };
        }
        let mut owner = Vec::with_capacity(lines.len());
        let mut current = 0usize;
        for line in &lines {
            let (time, rest) = split_time(line);
            if let Some(title) = rest.strip_prefix("##[group]")
                && let Some(next) = ordered
                    .iter()
                    .enumerate()
                    .skip(current + 1)
                    .find(|(_, step)| step.name == title.trim())
                    .map(|(index, _)| index)
            {
                current = next;
            } else if let Some(at) = time.and_then(crate::forge::cache::unix_seconds) {
                while current + 1 < ordered.len() {
                    let next = &ordered[current + 1];
                    let started = seconds(&next.started);
                    let done = seconds(&ordered[current].completed).is_none_or(|end| end < at);
                    if started.is_some_and(|started| started <= at) && done {
                        current += 1;
                    } else {
                        break;
                    }
                }
            }
            owner.push(current);
        }
        let mut out = Vec::new();
        let mut line = 0usize;
        for (index, step) in ordered.iter().enumerate() {
            let start = line;
            while line < owner.len() && owner[line] == index {
                line += 1;
            }
            out.push(LogSection {
                name: step.name.clone(),
                step: Some(step.number),
                start,
                end: line,
                depth: 0,
                started: step.started.clone(),
                completed: step.completed.clone(),
                collapsed: false,
            });
        }
        if let Some(last) = out.last_mut() {
            last.end = owner.len();
        }
        out
    }

    /// GitLab: one step per top-level trace section.
    pub fn steps(sections: &[LogSection]) -> Vec<Step> {
        sections
            .iter()
            .filter(|section| section.depth == 0)
            .enumerate()
            .map(|(index, section)| Step {
                number: index as u64 + 1,
                name: section.name.clone(),
                status: super::RunStatus::Completed,
                conclusion: None,
                conclusion_raw: None,
                started: section.started.clone(),
                completed: section.completed.clone(),
            })
            .collect()
    }

    fn line_time(line: &str) -> Option<&str> {
        split_time(line).0
    }

    /// `2026-11-14T04:22:04.5239335Z rest` (a BOM may lead the first line).
    fn split_time(line: &str) -> (Option<&str>, &str) {
        let line = line.trim_start_matches('\u{feff}');
        match line.split_once(' ') {
            Some((head, rest))
                if head.len() >= 20
                    && head.as_bytes()[4] == b'-'
                    && head.as_bytes()[10] == b'T'
                    && head.ends_with('Z') =>
            {
                (Some(head), rest)
            }
            _ => (None, line),
        }
    }

    /// An RFC 3339 UTC time from Unix seconds.
    pub fn rfc3339(seconds: i64) -> Timestamp {
        let days = seconds.div_euclid(86_400);
        let rest = seconds.rem_euclid(86_400);
        // Civil from days (Howard Hinnant).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Timestamp::new(format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            rest % 3600 / 60,
            rest % 60
        ))
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else if c != '\r' {
                out.push(c);
            }
        }
        out
    }

    /// `section_start:1763100004:step_script[collapsed=true]\r\x1b[0KHeader`
    fn marker<'a>(line: &'a str, word: &str) -> Option<(i64, &'a str, &'a str)> {
        let at = line.find(word)?;
        let rest = &line[at + word.len()..];
        let (time, rest) = rest.split_once(':')?;
        let time = time.parse().ok()?;
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        Some((time, &rest[..end], &rest[end..]))
    }

    fn gitlab(text: &str) -> Vec<LogSection> {
        let mut out: Vec<LogSection> = Vec::new();
        let mut open: Vec<(usize, String)> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if let Some((time, name, after)) = marker(line, "section_start:") {
                let (id, options) = match name.split_once('[') {
                    Some((id, options)) => (id, options),
                    None => (name, ""),
                };
                let header = strip_ansi(after);
                let header = header.trim();
                out.push(LogSection {
                    name: if header.is_empty() {
                        id.to_owned()
                    } else {
                        header.to_owned()
                    },
                    step: None,
                    start: index,
                    end: index + 1,
                    depth: open.len().min(u8::MAX as usize) as u8,
                    started: Some(rfc3339(time)),
                    completed: None,
                    collapsed: options.contains("collapsed=true"),
                });
                open.push((out.len() - 1, id.to_owned()));
            } else if let Some((time, name, _)) = marker(line, "section_end:")
                && let Some(position) = open.iter().rposition(|(_, id)| id == name)
            {
                for (slot, _) in open.drain(position..) {
                    out[slot].end = index + 1;
                    out[slot].completed = Some(rfc3339(time));
                }
            }
        }
        let total = text.lines().count();
        for (slot, _) in open {
            out[slot].end = total;
        }
        out
    }

    fn groups(text: &str) -> Vec<LogSection> {
        let mut out: Vec<LogSection> = Vec::new();
        let mut open: Option<usize> = None;
        for (index, line) in text.lines().enumerate() {
            let (time, rest) = split_time(line);
            if let Some(title) = rest.strip_prefix("##[group]") {
                if let Some(slot) = open.take() {
                    out[slot].end = index;
                }
                out.push(LogSection {
                    name: title.trim().to_owned(),
                    start: index,
                    end: index + 1,
                    started: time.map(Timestamp::new),
                    ..LogSection::default()
                });
                open = Some(out.len() - 1);
            } else if rest.starts_with("##[endgroup]")
                && let Some(slot) = open.take()
            {
                out[slot].end = index + 1;
                out[slot].completed = time.map(Timestamp::new);
            }
        }
        if let Some(slot) = open {
            out[slot].end = text.lines().count();
        }
        out
    }

    fn tabbed(text: &str) -> Vec<LogSection> {
        let mut out: Vec<LogSection> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let step = line.split('\t').nth(1).unwrap_or_default();
            match out.last_mut() {
                Some(last) if last.name == step => last.end = index + 1,
                _ => out.push(LogSection {
                    name: step.to_owned(),
                    start: index,
                    end: index + 1,
                    ..LogSection::default()
                }),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::forge::model::ItemId;
        use crate::forge::model::RunStatus;

        #[test]
        fn gitlab_sections_nest_and_carry_times() {
            let text = "\x1b[0KRunning\n\x1b[0Ksection_start:1763100000:prepare_script[collapsed=true]\r\x1b[0KPreparing environment\nrunner\n\x1b[0Ksection_end:1763100004:prepare_script\r\x1b[0K\n\x1b[0Ksection_start:1763100004:step_script\r\x1b[0K\x1b[32;1m$ cargo test\x1b[0;m\nok\n\x1b[0Ksection_end:1763100300:step_script\r\x1b[0K\n";
            let found = sections(text, LogFormat::GitLabSections);
            assert_eq!(
                found
                    .iter()
                    .map(|s| (s.name.as_str(), s.start, s.end, s.collapsed))
                    .collect::<Vec<_>>(),
                [
                    ("Preparing environment", 1, 4, true),
                    ("$ cargo test", 4, 7, false)
                ]
            );
            assert_eq!(
                found[0].started.as_ref().unwrap().as_str(),
                "2025-11-14T06:00:00Z"
            );
            assert_eq!(steps(&found).len(), 2);
        }

        #[test]
        fn github_logs_split_by_step_times_and_groups() {
            let text = "\u{feff}2026-11-08T10:00:00.1000000Z Current runner version\n2026-11-08T10:00:00.2000000Z ##[group]Run actions/checkout@v4\n2026-11-08T10:00:00.3000000Z checking out\n2026-11-08T10:00:00.4000000Z ##[endgroup]\n2026-11-08T10:00:02.0000000Z ##[group]Run cargo test\n2026-11-08T10:00:03.0000000Z test result: ok\n";
            let step = |number: u64, name: &str, started: &str, completed: &str| Step {
                number,
                name: name.into(),
                status: RunStatus::Completed,
                conclusion: None,
                conclusion_raw: None,
                started: Some(Timestamp::new(started)),
                completed: Some(Timestamp::new(completed)),
            };
            let log = Log::new(ItemId::new(1), text.into(), LogFormat::Plain, false);
            assert_eq!(
                log.sections
                    .iter()
                    .map(|s| (s.name.as_str(), s.start, s.end))
                    .collect::<Vec<_>>(),
                [("Run actions/checkout@v4", 1, 4), ("Run cargo test", 4, 6)]
            );
            let cut = split(
                &log,
                &[
                    step(
                        1,
                        "Set up job",
                        "2026-11-08T10:00:00Z",
                        "2026-11-08T10:00:00Z",
                    ),
                    step(
                        2,
                        "Run actions/checkout@v4",
                        "2026-11-08T10:00:00Z",
                        "2026-11-08T10:00:01Z",
                    ),
                    step(
                        3,
                        "Run cargo test",
                        "2026-11-08T10:00:02Z",
                        "2026-11-08T10:00:03Z",
                    ),
                    step(
                        4,
                        "Complete job",
                        "2026-11-08T10:00:04Z",
                        "2026-11-08T10:00:04Z",
                    ),
                ],
            );
            assert_eq!(
                cut.iter()
                    .map(|s| (s.step.unwrap(), s.start, s.end))
                    .collect::<Vec<_>>(),
                [(1, 0, 1), (2, 1, 4), (3, 4, 6), (4, 6, 6)]
            );
        }
    }
}

pub mod checks {
    //! Check rollup and dedupe (t3code): failing > action-required > pending > passing.
    use super::Check;
    use super::CiState;

    pub fn rollup(checks: &[Check]) -> Option<CiState> {
        if checks.is_empty() {
            return None;
        }
        let has = |state| checks.iter().any(|check| check.state == state);
        Some(if has(CiState::Failed) {
            CiState::Failed
        } else if has(CiState::ActionRequired) {
            CiState::ActionRequired
        } else if has(CiState::Pending) {
            CiState::Pending
        } else if checks
            .iter()
            .all(|check| matches!(check.state, CiState::Skipped | CiState::Cancelled))
        {
            CiState::Skipped
        } else {
            CiState::Passed
        })
    }

    /// One check per `(workflow, name)`, the newest start wins; `workflow / ` is prefixed only
    /// where two workflows share a check name.
    pub fn dedupe(checks: Vec<Check>) -> Vec<Check> {
        let mut kept: Vec<Check> = Vec::new();
        for check in checks {
            match kept
                .iter_mut()
                .find(|kept| kept.workflow == check.workflow && kept.name == check.name)
            {
                Some(existing) if existing.started < check.started => *existing = check,
                Some(_) => {}
                None => kept.push(check),
            }
        }
        let names = kept
            .iter()
            .map(|check| check.name.clone())
            .collect::<Vec<_>>();
        for check in &mut kept {
            if names.iter().filter(|name| **name == check.name).count() > 1
                && let Some(workflow) = &check.workflow
            {
                check.name = format!("{workflow} / {}", check.name);
            }
        }
        kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, workflow: &str, state: CiState, started: &str) -> Check {
        Check {
            name: name.into(),
            workflow: Some(workflow.into()),
            state,
            raw_state: None,
            started: Some(Timestamp::new(started)),
            duration: None,
            completed: None,
            details_url: None,
            run_link: None,
            required: false,
        }
    }

    #[test]
    fn rollup_orders_failing_before_pending_before_passing() {
        let passed = check("a", "w", CiState::Passed, "1");
        let pending = check("b", "w", CiState::Pending, "1");
        let failed = check("c", "w", CiState::Failed, "1");
        assert_eq!(checks::rollup(&[]), None);
        assert_eq!(
            checks::rollup(std::slice::from_ref(&passed)),
            Some(CiState::Passed)
        );
        assert_eq!(
            checks::rollup(&[passed.clone(), pending.clone()]),
            Some(CiState::Pending)
        );
        assert_eq!(
            checks::rollup(&[pending, failed, passed]),
            Some(CiState::Failed)
        );
    }

    #[test]
    fn dedupe_keeps_newest_and_prefixes_only_collisions() {
        let checks = checks::dedupe(vec![
            check("test", "ci", CiState::Failed, "2026-01-01T00:00:00Z"),
            check("test", "ci", CiState::Passed, "2026-01-02T00:00:00Z"),
            check("test", "nightly", CiState::Passed, "2026-01-01T00:00:00Z"),
            check("lint", "ci", CiState::Passed, "2026-01-01T00:00:00Z"),
        ]);
        let names = checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["ci / test", "nightly / test", "lint"]);
        assert_eq!(checks[0].state, CiState::Passed);
    }
}
