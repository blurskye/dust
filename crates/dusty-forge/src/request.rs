//! What the UI asks a forge for: `Read`s return a normalised `Payload`,
//! `Write`s return a `Receipt`. Both are provider-neutral; `provider/*.rs` maps them onto
//! `ApiRequest`s. Also the one error type every layer reports.

use crate::model::*;
use crate::pull_request::CreatePullRequest;
use crate::transport::Class;
use crate::transport::Cli;
use serde::Deserialize;
use serde::Serialize;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl StateFilter {
    /// The words the UI's state field uses (`open`, `closed`, `merged`, `all`).
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "closed" => StateFilter::Closed,
            "merged" => StateFilter::Merged,
            "all" => StateFilter::All,
            _ => StateFilter::Open,
        }
    }
}

/// A list filter. `query` is GitHub search syntax; F4/F5 translate it per provider.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Filter {
    pub state: StateFilter,
    pub query: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RunFilter {
    pub workflow: Option<ItemId>,
    pub branch: Option<String>,
    pub event: Option<String>,
    /// GitHub's status words (`action_required`, `in_progress`, `queued`, `failure`,
    /// `success`, `cancelled`); GitLab and Gitea translate them. `action_required` lists the
    /// fork runs waiting for approval.
    pub status: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NotifyFilter {
    pub all: bool,
    pub since: Option<Timestamp>,
}

/// A window of a list. `per_page` above the forge's page size (100 on GitHub) is served by
/// fetching consecutive pages, so `Page { number: 1, per_page: 300 }` is the first 300 rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Page {
    pub number: u32,
    pub per_page: u32,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            number: 1,
            per_page: 100,
        }
    }
}

impl Page {
    pub fn first(per_page: u32) -> Self {
        Self {
            number: 1,
            per_page,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageOf<T> {
    pub items: Vec<T>,
    pub next: Option<Page>,
    pub total: Option<u64>,
    /// How many items the same list holds in the other state (open ↔ closed), where the
    /// forge counts them in the same request (GitHub's `issueCount`); the `Closed` count
    /// of the list header before that list is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_total: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Read {
    Repository,
    /// Another repository on the same host (a fork's parent for the PR form).
    RepositoryNamed(String),
    Viewer,
    Labels,
    Milestones,
    Collaborators,
    List {
        kind: ItemKind,
        filter: Filter,
        page: Page,
    },
    /// Host-wide search (the inbox).
    Search {
        kind: ItemKind,
        filter: Filter,
        page: Page,
    },
    PullRequest(u64),
    /// Several pull requests at once (detail prefetch): one aliased GraphQL request per batch
    /// on GitHub, one read each elsewhere. Missing numbers are left out.
    PullRequests(Vec<u64>),
    Issue(u64),
    Fingerprint(u64),
    /// Several fingerprints at once (freshness polls), batched like `PullRequests`.
    Fingerprints(Vec<u64>),
    Timeline {
        kind: ItemKind,
        number: u64,
    },
    Threads(u64),
    Diff(u64),
    CommitDiff(String),
    Viewed(u64),
    PullRequestForBranch(String),
    Workflows,
    WorkflowSource {
        id: ItemId,
        reference: Option<String>,
    },
    Runs {
        filter: RunFilter,
        page: Page,
    },
    Run {
        id: ItemId,
        attempt: Option<u32>,
    },
    Job(ItemId),
    JobLog(ItemId),
    Artifacts(ItemId),
    /// The files inside an artifact archive (GitHub and Gitea artifact ids, GitLab job ids).
    ArtifactFiles(ItemId),
    /// One file of an artifact archive as text (lossy for binary files).
    ArtifactFile {
        artifact: ItemId,
        path: String,
    },
    Notifications {
        filter: NotifyFilter,
        page: Page,
    },
    PullRequestTemplate,
}

impl Read {
    /// Stable identity within one host/repo; F3's cache file name hashes this.
    pub fn cache_key(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Freshness budget before the payload is known; `cache::ttl` refines it by
    /// lifecycle (closed lists and items, running runs, quiet items).
    pub fn ttl(&self) -> Duration {
        const MINUTE: u64 = 60;
        const HOUR: u64 = 3600;
        let seconds = match self {
            Read::List { filter, .. } | Read::Search { filter, .. } => match filter.state {
                StateFilter::Closed | StateFilter::Merged => 10 * MINUTE,
                _ => MINUTE,
            },
            Read::PullRequest(_)
            | Read::PullRequests(_)
            | Read::Issue(_)
            | Read::Fingerprint(_)
            | Read::Fingerprints(_)
            | Read::Timeline { .. }
            | Read::Threads(_)
            | Read::Diff(_)
            | Read::Viewed(_)
            | Read::PullRequestForBranch(_) => 45,
            Read::CommitDiff(_)
            | Read::WorkflowSource {
                reference: Some(_), ..
            }
            | Read::JobLog(_)
            | Read::ArtifactFiles(_)
            | Read::ArtifactFile { .. } => 24 * HOUR,
            Read::Repository
            | Read::RepositoryNamed(_)
            | Read::Labels
            | Read::Milestones
            | Read::Collaborators
            | Read::PullRequestTemplate => HOUR,
            Read::Viewer => 24 * HOUR,
            Read::Workflows | Read::WorkflowSource { .. } | Read::Artifacts(_) => 10 * MINUTE,
            Read::Runs { .. } => 2 * MINUTE,
            Read::Run { .. } | Read::Job(_) => 8,
            Read::Notifications { .. } => MINUTE,
        };
        Duration::from_secs(seconds)
    }

    pub fn class(&self) -> Class {
        match self {
            Read::Fingerprint(_) | Read::Fingerprints(_) | Read::Notifications { .. } => {
                Class::Background
            }
            _ => Class::Interactive,
        }
    }

    pub fn number(&self) -> Option<u64> {
        match self {
            Read::PullRequest(number)
            | Read::Issue(number)
            | Read::Fingerprint(number)
            | Read::Timeline { number, .. }
            | Read::Threads(number)
            | Read::Diff(number)
            | Read::Viewed(number) => Some(*number),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct InlineDraft {
    pub path: String,
    pub line: u64,
    pub side: Side,
    pub body: String,
    /// The first line of a range comment (`line` is the last). GitHub anchors the range;
    /// GitLab and Gitea anchor on `line` and the body names the range (`range_body`).
    pub start_line: Option<u64>,
}

impl InlineDraft {
    /// The range a comment covers, when it covers more than one line.
    pub fn range(&self) -> Option<(u64, u64)> {
        self.start_line
            .filter(|start| *start < self.line)
            .map(|start| (start, self.line))
    }

    /// The body for a forge that anchors comments on one line: a range comment says which
    /// lines it is about first (`Lines 13–14:`), so the reviewer reads it in place.
    pub fn range_body(&self) -> String {
        match self.range() {
            Some((start, end)) => format!("**Lines {start}–{end}:** {}", self.body),
            None => self.body.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum RerunScope {
    All,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub enum SubscriptionState {
    Subscribed,
    Unsubscribed,
    Ignored,
}

impl SubscriptionState {
    pub fn as_github(self) -> &'static str {
        match self {
            SubscriptionState::Subscribed => "SUBSCRIBED",
            SubscriptionState::Unsubscribed => "UNSUBSCRIBED",
            SubscriptionState::Ignored => "IGNORED",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct RunIdentity {
    pub sha: String,
    pub branch: String,
    pub head_owner: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Write {
    CreateIssue {
        title: String,
        body: String,
        labels: Vec<String>,
        assignees: Vec<String>,
    },
    CreatePullRequest(CreatePullRequest),
    Edit {
        kind: ItemKind,
        number: u64,
        title: String,
        body: String,
    },
    Comment {
        kind: ItemKind,
        number: u64,
        body: String,
    },
    SetState {
        kind: ItemKind,
        number: u64,
        open: bool,
        reason: Option<ClosedReason>,
    },
    SetLabels {
        kind: ItemKind,
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    SetAssignees {
        kind: ItemKind,
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// `None` clears it; a title is resolved to the forge's milestone id.
    SetMilestone {
        kind: ItemKind,
        number: u64,
        milestone: Option<String>,
    },
    SetReviewers {
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// GitHub Projects stay on `gh … edit --add-project` (kept where already used).
    SetProjects {
        kind: ItemKind,
        number: u64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    Subscribe {
        kind: ItemKind,
        number: u64,
        node: ItemId,
        state: SubscriptionState,
    },
    InlineComment {
        number: u64,
        path: String,
        line: u64,
        side: Side,
        head: String,
        body: String,
    },
    Reply {
        number: u64,
        thread: ItemId,
        body: String,
    },
    Resolve {
        thread: ItemId,
        resolved: bool,
    },
    /// A review thread's suggested change committed to the pull request's head branch
    ///: lines `start..=end` of `path`, which read `original` when the
    /// suggestion was shown, become `replacement`. Refused when the head is no longer `head`
    /// or the lines changed.
    CommitSuggestion {
        number: u64,
        path: String,
        start: u64,
        end: u64,
        original: Vec<String>,
        replacement: Vec<String>,
        head: String,
        message: String,
    },
    SubmitReview {
        number: u64,
        verdict: Verdict,
        body: String,
        drafts: Vec<InlineDraft>,
        head: Option<String>,
    },
    Merge {
        number: u64,
        method: MergeMethod,
        head: String,
        delete_branch: bool,
        title: Option<String>,
        message: Option<String>,
    },
    /// Porcelain: `gh pr checkout`.
    Checkout {
        number: u64,
    },
    /// Porcelain: a sibling worktree, then `gh pr checkout` inside it.
    Worktree {
        number: u64,
    },
    /// Porcelain: `gh issue develop` (GitHub's linked branch has no REST endpoint).
    Develop {
        number: u64,
        branch: String,
        base: String,
    },
    SetViewed {
        number: u64,
        node: ItemId,
        path: String,
        viewed: bool,
    },
    Dispatch {
        workflow: ItemId,
        reference: String,
        inputs: Vec<(String, String)>,
    },
    Rerun {
        run: ItemId,
        scope: RerunScope,
        debug: bool,
    },
    RerunJob(ItemId),
    CancelRun(ItemId),
    ApproveRun {
        run: ItemId,
        expected: RunIdentity,
    },
    DownloadArtifact {
        artifact: ItemId,
        name: String,
        directory: std::path::PathBuf,
    },
    MarkRead(ItemId),
    MarkAllRead,
    Unsubscribe(ItemId),
    /// Porcelain `gh <kind> view N --web` when the item has no URL to hand the OS opener.
    OpenInBrowser {
        kind: ItemKind,
        number: u64,
        url: Option<String>,
    },
}

impl Write {
    /// The reads a confirmed write makes stale.
    pub fn invalidates(&self) -> Vec<Read> {
        let item = |kind: ItemKind, number: u64| match kind {
            ItemKind::Issue => Read::Issue(number),
            ItemKind::PullRequest => Read::PullRequest(number),
        };
        let timeline = |kind, number| Read::Timeline { kind, number };
        match self {
            Write::Edit { kind, number, .. }
            | Write::SetState { kind, number, .. }
            | Write::SetLabels { kind, number, .. }
            | Write::SetAssignees { kind, number, .. }
            | Write::SetMilestone { kind, number, .. }
            | Write::SetProjects { kind, number, .. }
            | Write::Subscribe { kind, number, .. }
            | Write::Comment { kind, number, .. } => {
                vec![item(*kind, *number), timeline(*kind, *number)]
            }
            Write::SetReviewers { number, .. } | Write::Merge { number, .. } => {
                vec![
                    Read::PullRequest(*number),
                    timeline(ItemKind::PullRequest, *number),
                ]
            }
            Write::SubmitReview { number, .. } => vec![
                Read::PullRequest(*number),
                timeline(ItemKind::PullRequest, *number),
                Read::Threads(*number),
            ],
            Write::InlineComment { number, .. } | Write::Reply { number, .. } => {
                vec![Read::Threads(*number)]
            }
            Write::CommitSuggestion { number, .. } => vec![
                Read::PullRequest(*number),
                timeline(ItemKind::PullRequest, *number),
                Read::Threads(*number),
                Read::Diff(*number),
            ],
            Write::SetViewed { number, .. } => vec![Read::Viewed(*number)],
            Write::Rerun { run, .. } | Write::CancelRun(run) | Write::ApproveRun { run, .. } => {
                vec![
                    Read::Run {
                        id: run.clone(),
                        attempt: None,
                    },
                    Read::Runs {
                        filter: RunFilter::default(),
                        page: Page::default(),
                    },
                ]
            }
            Write::Dispatch { .. } => vec![Read::Runs {
                filter: RunFilter::default(),
                page: Page::default(),
            }],
            _ => Vec::new(),
        }
    }

    /// Whether the UI may show the result before the forge confirms it.
    pub fn optimistic(&self) -> bool {
        matches!(
            self,
            Write::Comment { .. } | Write::Reply { .. } | Write::SetViewed { .. }
        )
    }

    /// Writes that leave the forge alone and act locally or in a browser.
    pub fn porcelain(&self) -> bool {
        matches!(
            self,
            Write::Checkout { .. }
                | Write::Worktree { .. }
                | Write::Develop { .. }
                | Write::SetProjects { .. }
                | Write::OpenInBrowser { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Repo(Repo),
    User(User),
    Labels(Vec<Label>),
    Milestones(Vec<Milestone>),
    Users(Vec<User>),
    Rows(PageOf<ItemRow>),
    PullRequest(Box<PullRequest>),
    Issue(Box<Issue>),
    Fingerprint(Fingerprint),
    PullRequests(Vec<PullRequest>),
    Fingerprints(Vec<(u64, Fingerprint)>),
    Timeline(Vec<TimelineEvent>),
    Threads(Vec<ReviewThread>),
    Text(String),
    Viewed(Vec<(String, bool)>),
    Workflows(Vec<Workflow>),
    Runs(PageOf<Run>),
    Run(Box<RunDetail>),
    Job(Box<Job>),
    Log(Log),
    Artifacts(Vec<Artifact>),
    ArtifactFiles(Vec<ArtifactFile>),
    Notifications(PageOf<Notification>),
    MaybePullRequest(Option<u64>),
    Template(Option<String>),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Receipt {
    pub summary: String,
    pub web_url: Option<String>,
    pub created_item: Option<(ItemKind, u64)>,
    #[serde(skip)]
    pub invalidates: Vec<Read>,
}

/// Every failure, from any transport or provider, as one of these. Display is the fixed
/// UI copy; messages are redacted before they get here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeError {
    CliMissing(Cli),
    CliTooOld {
        cli: Cli,
        found: String,
        needed: &'static str,
    },
    NotAuthenticated {
        host: String,
        hint: String,
    },
    /// The instance refuses every API call without a sign-in (Forgejo/Gitea
    /// `REQUIRE_SIGNIN_VIEW`, a private GitLab or GHES): reads cannot fall back to anonymous.
    SignInRequired {
        host: String,
        hints: Vec<String>,
    },
    /// `capability: "sign_in"` marks one read or write that needs a credential while the host
    /// is browsed anonymously (job logs, notifications, every write); `reason` is the full copy.
    Unsupported {
        capability: &'static str,
        reason: String,
    },
    NotFound(String),
    Forbidden(String),
    Conflict(String),
    RateLimited {
        until: Instant,
    },
    Network(String),
    Timeout,
    Cancelled,
    BadResponse(String),
    Failed(String),
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = |formatter: &mut std::fmt::Formatter<'_>, prefix: &str, message: &str| {
            if message.trim().is_empty() {
                formatter.write_str(prefix)
            } else {
                write!(formatter, "{prefix} · {}", message.trim())
            }
        };
        match self {
            ForgeError::CliMissing(cli) => write!(formatter, "{} is not installed", cli.name()),
            ForgeError::CliTooOld { cli, found, needed } => write!(
                formatter,
                "{} {found} is too old · {needed} or newer is needed",
                cli.name()
            ),
            ForgeError::NotAuthenticated { host, hint } => {
                if host.is_empty() {
                    write!(formatter, "Not signed in · run {hint}")
                } else {
                    write!(formatter, "Not signed in to {host} · run {hint}")
                }
            }
            ForgeError::SignInRequired { host, hints } => match hints.first() {
                Some(hint) => write!(formatter, "{host} requires sign-in · run {hint}"),
                None => write!(formatter, "{host} requires sign-in"),
            },
            ForgeError::Unsupported {
                capability: "sign_in",
                reason,
            } => formatter.write_str(reason.trim()),
            ForgeError::Unsupported { reason, .. } => detail(formatter, "Unsupported here", reason),
            ForgeError::NotFound(message) => detail(formatter, "Not found", message),
            ForgeError::Forbidden(message) => detail(formatter, "No permission", message),
            ForgeError::Conflict(message) => detail(formatter, "Conflict", message),
            ForgeError::RateLimited { until } => write!(
                formatter,
                "Rate limited · retry in {}s",
                until
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1)
            ),
            ForgeError::Network(message) => detail(formatter, "Network error", message),
            ForgeError::Timeout => formatter.write_str("Timed out waiting for the forge"),
            ForgeError::Cancelled => formatter.write_str("Cancelled"),
            ForgeError::BadResponse(message) => detail(formatter, "Unexpected response", message),
            ForgeError::Failed(message) => formatter.write_str(message.trim()),
        }
    }
}

impl std::error::Error for ForgeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_fixed_copy() {
        assert_eq!(
            ForgeError::NotAuthenticated {
                host: "github.com".into(),
                hint: "gh auth login --hostname github.com".into()
            }
            .to_string(),
            "Not signed in to github.com · run gh auth login --hostname github.com"
        );
        assert_eq!(ForgeError::NotFound(String::new()).to_string(), "Not found");
        assert_eq!(
            ForgeError::SignInRequired {
                host: "forgejo.example".into(),
                hints: vec!["tea login add --url https://forgejo.example".into()]
            }
            .to_string(),
            "forgejo.example requires sign-in · run tea login add --url https://forgejo.example"
        );
        assert_eq!(
            ForgeError::Conflict("head moved".into()).to_string(),
            "Conflict · head moved"
        );
    }

    #[test]
    fn reads_have_stable_keys_and_writes_name_what_they_invalidate() {
        let read = Read::List {
            kind: ItemKind::Issue,
            filter: Filter {
                state: StateFilter::parse("closed"),
                query: "label:bug".into(),
            },
            page: Page::default(),
        };
        assert_eq!(read.cache_key(), read.clone().cache_key());
        assert_ne!(read.cache_key(), Read::Issue(1).cache_key());
        let write = Write::Comment {
            kind: ItemKind::Issue,
            number: 7,
            body: "x".into(),
        };
        assert_eq!(
            write.invalidates(),
            [
                Read::Issue(7),
                Read::Timeline {
                    kind: ItemKind::Issue,
                    number: 7
                }
            ]
        );
        assert!(write.optimistic() && !write.porcelain());
    }
}
