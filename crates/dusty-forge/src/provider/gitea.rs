//! Gitea and Forgejo over `/api/v1`. One provider serves both: the
//! families share the issue/PR/review API and differ in Actions, which is gated on the server
//! version (`GET version`, cached per API base). Gitea gates on `1.N` (its main branch reports
//! `29.0.0+dev` and has everything); Forgejo gates on its own major, never on the `+gitea-1.22`
//! suffix every 13–17 release reports (tests/fixtures/forge/VERSIONS.md).
//!
//! Degradations travel as `Cap::No(reason)` and as `ForgeError::Unsupported` before any request
//! is sent: no thread resolution, no issue close reasons, no Projects, Actions per version.
//! Checks are commit statuses. "Viewed" file marks are kept on this computer.

use super::Conditional;
use super::Fetched;
use super::ForgeContext;
use super::Plan;
use super::Provider;
use super::ProviderKind;
use super::common::count;
use super::common::edited;
use super::common::id_of;
use super::common::is_bot;
use super::common::path_segment;
use super::common::repos_path;
use super::common::run_actor;
use super::common::span;
use super::common::text;
use super::parallel;
use super::reactions;
use crate::capability::Cap;
use crate::capability::Capabilities;
use crate::capability::GITEA_TERMS;
use crate::capability::RunFilters;
use crate::capability::StepsKind;
use crate::capability::Terms;
use crate::model::*;
use crate::request::*;
use crate::transport::Accept;
use crate::transport::ApiRequest;
use crate::transport::ApiResponse;
use crate::transport::Class;
use crate::transport::Method;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub struct Gitea;

/// Gitea's default `MAX_RESPONSE_ITEMS`; a server that caps lower is followed by `X-Total-Count`.
const PAGE: u32 = 50;
/// The log of a job Forgejo never ran: the screen shows it as a note, not as a log line.
pub const NEVER_RAN: &str = "This job never ran · no log";
/// Bounded fan-out for paginated detail reads.
const MAX_PAGES: u32 = 30;
/// The list's reading pane needs at most this much of a description.
const PREVIEW_BYTES: usize = 2048;

// ------------------------------------------------------------------------------ versions

/// What `GET version` said, reduced to what the gates compare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Server {
    Gitea {
        major: u32,
        minor: u32,
    },
    Forgejo {
        major: u32,
    },
    /// No answer (anonymous 403, old proxy): try the newest endpoint and fall back on 404.
    Unknown(ProviderKind),
}

impl Server {
    /// `1.22.6` → Gitea 1.22; `29.0.0+dev-160-g55186ca39b` → Gitea main;
    /// `17.0.0-26-363f3dc30b+gitea-1.22.0` → Forgejo 17.
    pub fn parse(version: &str) -> Option<Self> {
        let version = version.trim().trim_start_matches('v');
        let base = version.split('+').next().unwrap_or_default();
        let mut numbers = base.split(['.', '-']).map(str::parse::<u32>);
        let major = numbers.next()?.ok()?;
        let minor = numbers.next().and_then(Result::ok).unwrap_or(0);
        Some(match crate::detect::gitea_family(version) {
            ProviderKind::Forgejo => Server::Forgejo { major },
            _ => Server::Gitea { major, minor },
        })
    }

    pub fn kind(self) -> ProviderKind {
        match self {
            Server::Gitea { .. } => ProviderKind::Gitea,
            Server::Forgejo { .. } => ProviderKind::Forgejo,
            Server::Unknown(kind) => kind,
        }
    }

    /// `Some(true)`/`Some(false)` when the version decides, `None` when it is unknown and the
    /// family might have it.
    fn supports(self, gate: &Gate) -> Option<bool> {
        match self {
            Server::Gitea { major, minor } => {
                Some(gate.gitea.is_some_and(|need| (major, minor) >= need))
            }
            Server::Forgejo { major } => Some(gate.forgejo.is_some_and(|need| major >= need)),
            Server::Unknown(ProviderKind::Forgejo) if gate.forgejo.is_none() => Some(false),
            Server::Unknown(ProviderKind::Gitea) if gate.gitea.is_none() => Some(false),
            Server::Unknown(_) => None,
        }
    }

    fn cap(self, gate: &Gate) -> Cap {
        match self.supports(gate) {
            Some(true) => Cap::Yes,
            Some(false) => Cap::No(gate.reason(self.kind())),
            None => Cap::Probe,
        }
    }

    /// Whether to try the endpoint: `Ok(true)` known, `Ok(false)` unknown (probe it),
    /// `Err` when the version rules it out; no request is sent then.
    fn require(self, gate: &Gate) -> Result<bool, ForgeError> {
        match self.supports(gate) {
            Some(true) => Ok(true),
            None => Ok(false),
            Some(false) => Err(ForgeError::Unsupported {
                capability: gate.capability,
                reason: match self {
                    Server::Unknown(_) => gate.reason(self.kind()).to_owned(),
                    _ => format!("{} · this server runs {}", gate.reason(self.kind()), self),
                },
            }),
        }
    }
}

impl std::fmt::Display for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Server::Gitea { major, minor } if *major >= 2 => {
                write!(formatter, "Gitea {major}.{minor} (main)")
            }
            Server::Gitea { major, minor } => write!(formatter, "Gitea {major}.{minor}"),
            Server::Forgejo { major } => write!(formatter, "Forgejo {major}"),
            Server::Unknown(kind) => write!(formatter, "{} (version unknown)", kind.name()),
        }
    }
}

/// The first release with an endpoint (VERSIONS.md "[p] resolutions"). `None`: never.
struct Gate {
    capability: &'static str,
    gitea: Option<(u32, u32)>,
    forgejo: Option<u32>,
    gitea_reason: &'static str,
    forgejo_reason: &'static str,
}

impl Gate {
    fn reason(&self, kind: ProviderKind) -> &'static str {
        if kind == ProviderKind::Forgejo {
            self.forgejo_reason
        } else {
            self.gitea_reason
        }
    }
}

/// Gitea's main branch (`29.0.0+dev`): anything numbered past 1.x.
const MAIN: (u32, u32) = (2, 0);

const TASKS: Gate = Gate {
    capability: "actions",
    gitea: Some((1, 23)),
    forgejo: Some(8),
    gitea_reason: "Actions needs Gitea 1.23 or newer · o opens Actions in the browser",
    forgejo_reason: "Actions needs Forgejo 8 or newer · o opens Actions in the browser",
};
const RUNS: Gate = Gate {
    capability: "runs_list",
    gitea: Some((1, 25)),
    forgejo: Some(12),
    gitea_reason: "The runs list needs Gitea 1.25 or newer",
    forgejo_reason: "The runs list needs Forgejo 12 or newer",
};
const RUN_JOBS: Gate = Gate {
    capability: "run_jobs",
    gitea: Some((1, 25)),
    forgejo: Some(16),
    gitea_reason: "Run jobs need Gitea 1.25 or newer",
    forgejo_reason: "Run jobs need Forgejo 16 or newer",
};
const JOB: Gate = Gate {
    capability: "job",
    gitea: Some((1, 25)),
    forgejo: Some(17),
    gitea_reason: "Job details need Gitea 1.25 or newer · o opens the run in the browser",
    forgejo_reason: "Job details need Forgejo 17 or newer · o opens the run in the browser",
};
const JOB_LOG: Gate = Gate {
    capability: "job_log",
    gitea: Some((1, 24)),
    forgejo: Some(16),
    gitea_reason: "Job logs need Gitea 1.24 or newer · o opens the run in the browser",
    forgejo_reason: "Job logs need Forgejo 16 or newer · o opens the run in the browser",
};
const WORKFLOWS: Gate = Gate {
    capability: "workflows",
    gitea: Some((1, 24)),
    forgejo: None,
    gitea_reason: "The workflow list needs Gitea 1.24 or newer",
    forgejo_reason: "Forgejo has no workflow list · runs show their workflow file",
};
const RERUN: Gate = Gate {
    capability: "rerun_all",
    gitea: Some((1, 26)),
    forgejo: Some(17),
    gitea_reason: "Rerunning needs Gitea 1.26 or newer · o opens the run in the browser",
    forgejo_reason: "Rerunning needs Forgejo 17 or newer · o opens the run in the browser",
};
const RERUN_FAILED: Gate = Gate {
    capability: "rerun_failed",
    gitea: Some((1, 26)),
    forgejo: None,
    gitea_reason: "Rerunning failed jobs needs Gitea 1.26 or newer",
    forgejo_reason: "Forgejo reruns whole runs only · R reruns all jobs",
};
const RERUN_JOB: Gate = Gate {
    capability: "rerun_job",
    gitea: Some((1, 26)),
    forgejo: Some(17),
    gitea_reason: "Rerunning one job needs Gitea 1.26 or newer",
    forgejo_reason: "Rerunning one job needs Forgejo 17 or newer",
};
const CANCEL: Gate = Gate {
    capability: "cancel_run",
    gitea: Some(MAIN),
    forgejo: Some(16),
    gitea_reason: "Cancelling needs a Gitea newer than 1.27 · o opens the run in the browser",
    forgejo_reason: "Cancelling needs Forgejo 16 or newer · o opens the run in the browser",
};
const APPROVE: Gate = Gate {
    capability: "approve_fork_run",
    gitea: Some(MAIN),
    forgejo: None,
    gitea_reason: "Approving fork runs needs a Gitea newer than 1.27 · o opens the run",
    forgejo_reason: "Forgejo approves fork runs in the web UI only · o opens the run",
};
const ATTEMPTS: Gate = Gate {
    capability: "attempts",
    gitea: Some((1, 27)),
    forgejo: None,
    gitea_reason: "Earlier attempts need Gitea 1.27 or newer",
    forgejo_reason: "Forgejo shows the latest attempt only",
};
const DISPATCH: Gate = Gate {
    capability: "dispatch",
    gitea: Some((1, 24)),
    forgejo: Some(8),
    gitea_reason: "Running a workflow needs Gitea 1.24 or newer",
    forgejo_reason: "Running a workflow needs Forgejo 8 or newer",
};
const ARTIFACTS: Gate = Gate {
    capability: "artifacts_list",
    gitea: Some((1, 24)),
    forgejo: Some(16),
    gitea_reason: "Artifacts need Gitea 1.24 or newer · o opens the run in the browser",
    forgejo_reason: "Artifacts need Forgejo 16 or newer · o opens the run in the browser",
};
const PR_FOR_BRANCH: Gate = Gate {
    capability: "pull_request_for_branch",
    gitea: Some((1, 22)),
    forgejo: Some(7),
    gitea_reason: "",
    forgejo_reason: "",
};

const RESOLVE_REASON: &str =
    "Gitea and Forgejo have no API to resolve conversations · o opens the thread";
const PROJECTS_REASON: &str = "Projects are managed in the web UI here · o opens the item";
const NOT_PLANNED_REASON: &str = "Issues close without a reason here";

fn servers() -> &'static Mutex<HashMap<String, Server>> {
    static SERVERS: OnceLock<Mutex<HashMap<String, Server>>> = OnceLock::new();
    SERVERS.get_or_init(Default::default)
}

/// The server behind a context, asked once per API base and process.
pub fn server(ctx: &ForgeContext, cancel: &AtomicBool) -> Server {
    let base = ctx.endpoints.api_base.clone();
    if let Some(server) = servers()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&base)
    {
        return *server;
    }
    let request = ApiRequest::get("version").timeout(crate::detect::PROBE_TIMEOUT * 3);
    let server = match ctx.send(&request, cancel) {
        Ok(response) => response
            .success()
            .then(|| response.json().ok())
            .flatten()
            .and_then(|value| value["version"].as_str().and_then(Server::parse))
            .unwrap_or(Server::Unknown(ctx.endpoints.kind)),
        // Unreachable: decide on the next request, not for the whole process.
        Err(_) => return Server::Unknown(ctx.endpoints.kind),
    };
    servers()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(base, server);
    server
}

/// Gitea/Forgejo capabilities for a server version .
pub fn capabilities(server: Server) -> Capabilities {
    let tasks = server.cap(&TASKS);
    let runs = match server.cap(&RUNS) {
        Cap::No(_) => tasks.clone(),
        cap => cap,
    };
    let run_jobs = match server.cap(&RUN_JOBS) {
        Cap::No(_) => tasks.clone(),
        cap => cap,
    };
    // Gitea ≥ 1.25 sends steps with the run's jobs; Forgejo only on the single job (≥ 17).
    let steps = match server.supports(&JOB) {
        Some(false) => StepsKind::Unavailable,
        _ => StepsKind::Known,
    };
    Capabilities {
        pull_requests: Cap::Yes,
        issues: Cap::Yes,
        actions: tasks,
        notifications: Cap::Yes,
        cross_repo_search: Cap::Yes,
        review_threads: Cap::Yes,
        resolve_thread: Cap::No(RESOLVE_REASON),
        reply_in_thread: Cap::Yes,
        inline_comment: Cap::Yes,
        batched_review: Cap::Yes,
        review_approve: Cap::Yes,
        review_request_changes: Cap::Yes,
        // Kept on this computer: the API has no viewed state.
        viewed_files: Cap::Yes,
        merge_methods: vec![
            MergeMethod::Merge,
            MergeMethod::Rebase,
            MergeMethod::RebaseMerge,
            MergeMethod::Squash,
            MergeMethod::FastForwardOnly,
        ],
        merge_pinned_head: Cap::Yes,
        delete_branch_on_merge: Cap::Yes,
        auto_merge: Cap::No("Auto-merge is scheduled in the web UI here · o opens it"),
        draft_pr: Cap::Yes,
        create_pr_cross_fork: Cap::Yes,
        update_branch: Cap::Yes,
        issue_not_planned: Cap::No(NOT_PLANNED_REASON),
        labels: Cap::Yes,
        assignees: Cap::Yes,
        milestones: Cap::Yes,
        reviewers: Cap::Yes,
        projects: Cap::No(PROJECTS_REASON),
        subscribe: Cap::Yes,
        // Read from the description's closing keywords (`closing.rs`).
        linked_issues: Cap::Yes,
        runs_list: runs,
        run_jobs,
        job_steps: steps,
        job_log: server.cap(&JOB_LOG),
        rerun_all: server.cap(&RERUN),
        rerun_failed: server.cap(&RERUN_FAILED),
        rerun_job: server.cap(&RERUN_JOB),
        cancel_run: server.cap(&CANCEL),
        dispatch: server.cap(&DISPATCH),
        dispatch_inputs_schema: server.cap(&DISPATCH),
        approve_fork_run: server.cap(&APPROVE),
        attempts: server.cap(&ATTEMPTS),
        artifacts_list: server.cap(&ARTIFACTS),
        artifacts_download: server.cap(&ARTIFACTS),
        run_filters: RunFilters {
            workflow: true,
            branch: true,
            event: true,
            status: true,
        },
        etag: Cap::No("Gitea and Forgejo send no ETags"),
        since_queries: Cap::Yes,
        graphql_batching: Cap::No("Gitea and Forgejo have no GraphQL API"),
        writes: Cap::Yes,
    }
}

// ------------------------------------------------------------------------------ mapping

/// Gitea sends an unset time as `0001-01-01T00:00:00Z`.
fn optional(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty() && !text.starts_with("0001-01-01"))
        .map(str::to_owned)
}

fn user(value: &Value) -> User {
    let login = optional(value, "login").or_else(|| optional(value, "username"));
    User {
        login: login.unwrap_or_default(),
        id: value["id"].as_u64().map(ItemId::new),
        name: optional(value, "full_name"),
        bot: is_bot(value),
    }
}

fn users(value: &Value) -> Vec<User> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(user)
        .filter(|user| !user.login.is_empty())
        .collect()
}

fn label(value: &Value) -> Label {
    Label {
        id: value["id"].as_u64().map(ItemId::new),
        name: text(value, "name"),
        color: optional(value, "color").map(|color| color.trim_start_matches('#').to_owned()),
    }
}

fn labels(value: &Value) -> Vec<Label> {
    value.as_array().into_iter().flatten().map(label).collect()
}

fn timestamp(value: &Value, key: &str) -> Timestamp {
    Timestamp::new(optional(value, key).unwrap_or_default())
}

fn optional_time(value: &Value, key: &str) -> Option<Timestamp> {
    optional(value, key).map(Timestamp::new)
}

fn milestone(value: &Value) -> Option<Milestone> {
    value.is_object().then(|| Milestone {
        id: value["id"].as_u64().map(ItemId::new),
        title: text(value, "title"),
        due: optional_time(value, "due_on"),
        open: value["state"]
            .as_str()
            .is_none_or(|state| state.eq_ignore_ascii_case("open")),
    })
}

fn preview(body: &str) -> String {
    if body.len() <= PREVIEW_BYTES {
        return body.to_owned();
    }
    let mut end = PREVIEW_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_owned()
}

fn wip(title: &str) -> bool {
    let title = title.trim_start().to_ascii_uppercase();
    title.starts_with("WIP:") || title.starts_with("[WIP]")
}

/// Commit status words (`pending`, `success`, `error`, `failure`, `warning`, `skipped`).
pub fn status_state(word: &str) -> CiState {
    match word.to_ascii_lowercase().as_str() {
        "success" => CiState::Passed,
        "failure" | "error" => CiState::Failed,
        "pending" | "running" | "waiting" | "blocked" => CiState::Pending,
        "skipped" => CiState::Skipped,
        "cancelled" => CiState::Cancelled,
        _ => CiState::Unknown,
    }
}

/// Forgejo runs and every `actions/tasks` row carry one word for status and result.
fn result_word(word: &str) -> (RunStatus, Option<CiState>) {
    match word.to_ascii_lowercase().as_str() {
        "success" => (RunStatus::Completed, Some(CiState::Passed)),
        "failure" => (RunStatus::Completed, Some(CiState::Failed)),
        "cancelled" => (RunStatus::Completed, Some(CiState::Cancelled)),
        "skipped" => (RunStatus::Completed, Some(CiState::Skipped)),
        "running" => (RunStatus::InProgress, None),
        "blocked" => (RunStatus::Waiting, None),
        _ => (RunStatus::Queued, None),
    }
}

/// Gitea's GitHub-shaped Actions (`status` + `conclusion`).
fn github_status(word: &str) -> RunStatus {
    match word {
        "in_progress" | "running" => RunStatus::InProgress,
        "completed" => RunStatus::Completed,
        "waiting" | "blocked" => RunStatus::Waiting,
        "requested" => RunStatus::Requested,
        "pending" => RunStatus::Pending,
        _ => RunStatus::Queued,
    }
}

fn conclusion_state(word: &str) -> CiState {
    match word.to_ascii_lowercase().as_str() {
        "success" => CiState::Passed,
        "failure" | "timed_out" | "startup_failure" => CiState::Failed,
        "cancelled" => CiState::Cancelled,
        "skipped" | "neutral" => CiState::Skipped,
        "action_required" => CiState::ActionRequired,
        _ => CiState::Unknown,
    }
}

fn lifecycle(value: &Value) -> Lifecycle {
    let pull = &value["pull_request"];
    let merged = value["merged"].as_bool() == Some(true) || pull["merged"].as_bool() == Some(true);
    if merged {
        return Lifecycle::Merged;
    }
    if text(value, "state").eq_ignore_ascii_case("closed") {
        return Lifecycle::Closed;
    }
    if draft(value) {
        Lifecycle::Draft
    } else {
        Lifecycle::Open
    }
}

/// Once a pull request's head branch is deleted, Gitea and Forgejo report `ref` as
/// `refs/pull/<N>/head` and keep the branch only in `label`; an AGit pull request (`flow` 1)
/// or a deleted fork keeps the ref (t3code `forgejoHeadBranch`).
fn head_branch(value: &Value, reference: String) -> String {
    let head = &value["head"];
    match optional(head, "label") {
        Some(label)
            if reference.starts_with("refs/pull/")
                && !head["repo"].is_null()
                && value["flow"].as_u64() != Some(1) =>
        {
            label
        }
        _ => reference,
    }
}

fn draft(value: &Value) -> bool {
    value["draft"].as_bool() == Some(true)
        || value["pull_request"]["draft"].as_bool() == Some(true)
        || (is_pull(value) && wip(&text(value, "title")))
}

fn is_pull(value: &Value) -> bool {
    value.get("head").is_some() || value["pull_request"].is_object()
}

/// A row from `pulls`, `issues` or `repos/issues/search`.
pub fn row(value: &Value) -> ItemRow {
    let kind = if is_pull(value) {
        ItemKind::PullRequest
    } else {
        ItemKind::Issue
    };
    ItemRow {
        kind: Some(kind),
        number: count(value, "number"),
        id: id_of(value, "id"),
        title: text(value, "title"),
        author: user(&value["user"]),
        lifecycle: lifecycle(value),
        labels: labels(&value["labels"]),
        updated: timestamp(value, "updated_at"),
        comments: count(value, "comments"),
        repo: optional(&value["repository"], "full_name")
            .or_else(|| optional(&value["base"]["repo"], "full_name")),
        web_url: text(value, "html_url"),
        // A `pulls` list object names the reviewers still asked; that is the only review
        // state a list carries (verdicts need the reviews endpoint).
        review: value["requested_reviewers"]
            .as_array()
            .is_some_and(|reviewers| !reviewers.is_empty())
            .then_some(ReviewState::Requested),
        ci: None,
        ci_counts: Vec::new(),
        additions: value["additions"].as_u64(),
        deletions: value["deletions"].as_u64(),
        changed_files: value["changed_files"].as_u64(),
        draft: kind == ItemKind::PullRequest && draft(value),
        body_preview: value["body"].as_str().map(preview),
        head_branch: optional(&value["head"], "ref").map(|reference| head_branch(value, reference)),
        review_tally: None,
        created: Some(timestamp(value, "created_at")).filter(|created| !created.is_empty()),
        linked: None,
    }
}

fn review_state(review: &Value) -> ReviewState {
    if review["dismissed"].as_bool() == Some(true) {
        return ReviewState::Dismissed;
    }
    match text(review, "state").to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewState::Approved,
        "REQUEST_CHANGES" | "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENT" | "COMMENTED" => ReviewState::Commented,
        "PENDING" => ReviewState::Pending,
        "REQUEST_REVIEW" => ReviewState::Requested,
        _ => ReviewState::NotRequested,
    }
}

fn review(value: &Value) -> Review {
    Review {
        id: id_of(value, "id"),
        author: user(&value["user"]),
        state: review_state(value),
        body: text(value, "body"),
        submitted: timestamp(value, "submitted_at"),
    }
}

/// Requested reviewers, then each reviewer's latest verdict (a comment does not replace one).
fn reviewer_slots(pull: &Value, reviews: &[Review]) -> Vec<ReviewerSlot> {
    let mut slots: Vec<ReviewerSlot> = Vec::new();
    for review in reviews {
        if review.author.login.is_empty()
            || matches!(
                review.state,
                ReviewState::Pending | ReviewState::Requested | ReviewState::NotRequested
            )
        {
            continue;
        }
        match slots
            .iter_mut()
            .find(|slot| slot.reviewer == review.author.login)
        {
            Some(slot) if review.state != ReviewState::Commented => slot.state = review.state,
            Some(_) => {}
            None => slots.push(ReviewerSlot {
                reviewer: review.author.login.clone(),
                state: review.state,
            }),
        }
    }
    for requested in users(&pull["requested_reviewers"]) {
        match slots
            .iter_mut()
            .find(|slot| slot.reviewer == requested.login)
        {
            Some(slot) => slot.state = ReviewState::Requested,
            None => slots.push(ReviewerSlot {
                reviewer: requested.login,
                state: ReviewState::Requested,
            }),
        }
    }
    for team in pull["requested_reviewers_teams"]
        .as_array()
        .into_iter()
        .flatten()
    {
        slots.push(ReviewerSlot {
            reviewer: text(team, "name"),
            state: ReviewState::Requested,
        });
    }
    slots
}

fn decision(slots: &[ReviewerSlot]) -> Option<ReviewState> {
    let has = |state| slots.iter().any(|slot| slot.state == state);
    if has(ReviewState::ChangesRequested) {
        Some(ReviewState::ChangesRequested)
    } else if has(ReviewState::Approved) {
        Some(ReviewState::Approved)
    } else if has(ReviewState::Requested) {
        Some(ReviewState::Requested)
    } else {
        None
    }
}

fn web(ctx: &ForgeContext, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.is_empty() {
        url.to_owned()
    } else {
        format!(
            "{}/{}",
            ctx.endpoints.web_base.trim_end_matches('/'),
            url.trim_start_matches('/')
        )
    }
}

/// `…/actions/runs/{run}[/jobs/{job}]` in a status target URL.
fn run_link(url: &str) -> Option<RunLink> {
    let (_, tail) = url.split_once("/actions/runs/")?;
    let mut parts = tail.split('/');
    let run = parts.next().filter(|run| !run.is_empty())?;
    let job = match (parts.next(), parts.next()) {
        (Some("jobs"), Some(job)) if !job.is_empty() => Some(ItemId::new(job)),
        _ => None,
    };
    Some(RunLink {
        run: ItemId::new(run),
        job,
    })
}

/// A commit status as a check; the context's `workflow / job (event)` stays its name.
fn status_check(ctx: &ForgeContext, value: &Value) -> Check {
    let word = optional(value, "status")
        .or_else(|| optional(value, "state"))
        .unwrap_or_default();
    let state = status_state(&word);
    let url = web(ctx, &text(value, "target_url"));
    Check {
        name: text(value, "context"),
        workflow: None,
        state,
        raw_state: Some(word.to_ascii_uppercase()),
        started: optional_time(value, "created_at"),
        duration: None,
        completed: (state != CiState::Pending)
            .then(|| optional_time(value, "updated_at"))
            .flatten(),
        run_link: run_link(&url),
        details_url: Some(url).filter(|url| !url.is_empty()),
        required: false,
    }
}

fn ci_counts(checks: &[Check]) -> Vec<(CiState, u64)> {
    let mut counts: Vec<(CiState, u64)> = Vec::new();
    for check in checks {
        match counts.iter_mut().find(|(state, _)| *state == check.state) {
            Some((_, total)) => *total += 1,
            None => counts.push((check.state, 1)),
        }
    }
    counts
}

fn file_status(word: &str) -> FileStatus {
    match word.to_ascii_lowercase().as_str() {
        "added" => FileStatus::Added,
        "deleted" | "removed" => FileStatus::Deleted,
        "renamed" => FileStatus::Renamed,
        "copied" => FileStatus::Copied,
        _ => FileStatus::Modified,
    }
}

fn changed_file(value: &Value, viewed: &BTreeMap<String, bool>) -> ChangedFile {
    let path = text(value, "filename");
    ChangedFile {
        viewed: Some(viewed.get(&path).copied().unwrap_or(false)),
        old_path: optional(value, "previous_filename"),
        status: file_status(&text(value, "status")),
        additions: count(value, "additions"),
        deletions: count(value, "deletions"),
        path,
    }
}

fn commit(value: &Value) -> CommitSummary {
    let message = text(&value["commit"], "message");
    let author = user(&value["author"]);
    CommitSummary {
        sha: text(value, "sha"),
        headline: message.lines().next().unwrap_or_default().to_owned(),
        body: message
            .split_once('\n')
            .map(|(_, body)| body.trim().to_owned())
            .unwrap_or_default(),
        committed: optional_time(&value["commit"]["committer"], "date")
            .or_else(|| optional_time(value, "created"))
            .unwrap_or_default(),
        author: Some(author)
            .filter(|author| !author.login.is_empty())
            .or_else(|| optional(&value["commit"]["author"], "name").map(User::login)),
    }
}

fn mergeable(pull: &Value) -> Mergeable {
    if pull["merged"].as_bool() == Some(true) || text(pull, "state") == "closed" {
        Mergeable::Unknown
    } else if draft(pull) {
        Mergeable::Draft
    } else {
        match pull["mergeable"].as_bool() {
            Some(true) => Mergeable::Clean,
            Some(false) => Mergeable::Conflicting,
            None => Mergeable::Unknown,
        }
    }
}

fn fingerprint(pull: &Value, rollup: Option<CiState>) -> Fingerprint {
    Fingerprint {
        state: if pull["merged"].as_bool() == Some(true) {
            "MERGED".into()
        } else {
            text(pull, "state").to_ascii_uppercase()
        },
        mergeable: match pull["mergeable"].as_bool() {
            Some(true) => "MERGEABLE",
            Some(false) => "CONFLICTING",
            None => "UNKNOWN",
        }
        .into(),
        head: text(&pull["head"], "sha"),
        rollup,
        updated: timestamp(pull, "updated_at"),
    }
}

fn ref_info(value: &Value) -> RefInfo {
    RefInfo {
        repo: optional(&value["repo"], "full_name"),
        branch: text(value, "ref"),
        sha: text(value, "sha"),
    }
}

fn issue(value: &Value) -> Issue {
    Issue {
        row: row(value),
        body: text(value, "body"),
        assignees: users(&value["assignees"]),
        milestone: milestone(&value["milestone"]),
        linked_prs: Vec::new(),
        created: timestamp(value, "created_at"),
        closed_reason: None,
        projects: Vec::new(),
        subscribed: None,
        reactions: Vec::new(),
    }
}

/// Review comments grouped into threads by file, side and line: Gitea has no thread object;
/// its UI groups the same way. A thread is resolved when its comments carry a resolver.
pub fn threads(comments: &[Value]) -> Vec<ReviewThread> {
    let mut threads: Vec<ReviewThread> = Vec::new();
    let mut sorted = comments.to_vec();
    sorted.sort_by(|left, right| {
        text(left, "created_at")
            .cmp(&text(right, "created_at"))
            .then(count(left, "id").cmp(&count(right, "id")))
    });
    for value in &sorted {
        let path = text(value, "path");
        let (side, line) = match (count(value, "position"), count(value, "original_position")) {
            (0, 0) => (Side::Right, 0),
            (0, old) => (Side::Left, old),
            (new, _) => (Side::Right, new),
        };
        let key = thread_id(side, line, &path, None);
        let commit = optional(value, "commit_id").or_else(|| optional(value, "original_commit_id"));
        let comment = Comment {
            id: id_of(value, "id"),
            author: user(&value["user"]),
            body: text(value, "body"),
            created: timestamp(value, "created_at"),
            system: false,
            pending: false,
            position: Some(CommentPosition {
                path: path.clone(),
                line: (side == Side::Right && line > 0).then_some(line),
                original_line: (side == Side::Left).then_some(line),
                commit: commit.clone(),
            }),
            // A review comment's `updated_at` also moves when the diff under it does.
            ..Comment::default()
        };
        let resolved = value["resolver"].is_object();
        let same = |thread: &ReviewThread| {
            let line = thread.line.or(thread.original_line).unwrap_or(0);
            thread_id(
                thread.side,
                line,
                thread.path.as_deref().unwrap_or(""),
                None,
            ) == key
        };
        match threads.iter_mut().find(|thread| same(thread)) {
            Some(thread) => {
                thread.resolved = Some(thread.resolved == Some(true) || resolved);
                thread.comments.push(comment);
            }
            None => threads.push(ReviewThread {
                id: ItemId::new(thread_id(side, line, &path, commit.as_deref())),
                resolved: Some(resolved),
                resolvable: false,
                path: Some(path).filter(|path| !path.is_empty()),
                line: (side == Side::Right && line > 0).then_some(line),
                original_line: (side == Side::Left).then_some(line),
                side,
                commit,
                comments: vec![comment],
                total_comments: None,
                diff_hunk: optional(value, "diff_hunk"),
                start_line: None,
            }),
        }
    }
    threads
}

/// `R12@<commit>:src/lib.rs` — side, line, the thread's first commit and path: everything a
/// reply needs to land in the same conversation.
fn thread_id(side: Side, line: u64, path: &str, commit: Option<&str>) -> String {
    let side = if side == Side::Left { 'L' } else { 'R' };
    match commit {
        Some(commit) => format!("{side}{line}@{commit}:{path}"),
        None => format!("{side}{line}:{path}"),
    }
}

fn parse_thread(id: &str) -> Option<(Side, u64, Option<String>, String)> {
    let (position, path) = id.split_once(':')?;
    let (position, commit) = match position.split_once('@') {
        Some((position, commit)) => (position, Some(commit.to_owned())),
        None => (position, None),
    };
    let side = match position.chars().next()? {
        'L' => Side::Left,
        'R' => Side::Right,
        _ => return None,
    };
    let line = position[1..].parse().ok()?;
    (!path.is_empty()).then(|| (side, line, commit, path.to_owned()))
}

fn actor(event: &Value) -> Actor {
    Actor {
        actor: user(&event["user"]),
        at: timestamp(event, "created_at"),
    }
}

/// One `issues/{n}/timeline` entry. `reviews` maps review ids to their verdicts (the timeline
/// entry carries only the id).
fn timeline_event(event: &Value, reviews: &HashMap<u64, Review>) -> Option<TimelineEvent> {
    let by = actor(event);
    let kind = text(event, "type");
    Some(match kind.as_str() {
        "comment" => TimelineEvent::Commented(Comment {
            id: id_of(event, "id"),
            author: by.actor,
            body: text(event, "body"),
            created: by.at,
            system: false,
            pending: false,
            position: None,
            association: None,
            edited: edited(event, "created_at", "updated_at"),
            reactions: Vec::new(),
        }),
        // Inline review comments belong to threads.
        "code" => return None,
        "review" => {
            let known = reviews.get(&count(event, "review_id"));
            TimelineEvent::Reviewed(Review {
                id: known
                    .map(|review| review.id.clone())
                    .unwrap_or_else(|| id_of(event, "review_id")),
                state: known.map_or(ReviewState::Commented, |review| review.state),
                body: optional(event, "body")
                    .or_else(|| known.map(|review| review.body.clone()))
                    .unwrap_or_default(),
                author: by.actor,
                submitted: by.at,
            })
        }
        "label" => {
            let label = text(&event["label"], "name");
            if text(event, "body") == "1" {
                TimelineEvent::Labeled { label, by }
            } else {
                TimelineEvent::Unlabeled { label, by }
            }
        }
        "assignees" => {
            let assignee = user(&event["assignee"]).login;
            if event["removed_assignee"].as_bool() == Some(true) {
                TimelineEvent::Unassigned { assignee, by }
            } else {
                TimelineEvent::Assigned { assignee, by }
            }
        }
        "review_request" => TimelineEvent::ReviewRequested {
            reviewer: optional(&event["assignee"], "login")
                .or_else(|| optional(&event["assignee_team"], "name"))
                .unwrap_or_default(),
            by,
        },
        "close" => TimelineEvent::Closed { by },
        "reopen" => TimelineEvent::Reopened { by },
        // `milestone` set, `old_milestone` cleared (both on a change).
        "milestone" => match (
            optional(&event["milestone"], "title"),
            optional(&event["old_milestone"], "title"),
        ) {
            (Some(milestone), _) => TimelineEvent::Milestoned { milestone, by },
            (None, Some(milestone)) => TimelineEvent::Demilestoned { milestone, by },
            (None, None) => TimelineEvent::Other {
                kind: "milestone".into(),
                by,
            },
        },
        "merge_pull" => TimelineEvent::Merged {
            sha: optional(event, "ref_commit_sha").unwrap_or_default(),
            by,
        },
        "change_title" => TimelineEvent::Renamed {
            from: text(event, "old_title"),
            to: text(event, "new_title"),
            by,
        },
        "pull_push" => {
            let push: Value = serde_json::from_str(&text(event, "body")).unwrap_or_default();
            let commits = push["commit_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if push["is_force_push"].as_bool() == Some(true) {
                TimelineEvent::ForcePushed {
                    from: commits.first().copied().unwrap_or_default().into(),
                    to: commits.last().copied().unwrap_or_default().into(),
                    by,
                }
            } else {
                TimelineEvent::Other {
                    kind: match commits.len() {
                        1 => "pushed 1 commit".into(),
                        n => format!("pushed {n} commits"),
                    },
                    by,
                }
            }
        }
        "pull_ref" | "issue_ref" | "comment_ref" => {
            let issue = &event["ref_issue"];
            if !issue.is_object() {
                return Some(TimelineEvent::Other {
                    kind: kind.replace('_', " "),
                    by,
                });
            }
            TimelineEvent::CrossReferenced {
                link: LinkRef {
                    number: count(issue, "number"),
                    title: text(issue, "title"),
                    repo: optional(&issue["repository"], "full_name"),
                    web_url: optional(issue, "html_url"),
                    kind: Some(if issue["pull_request"].is_object() {
                        ItemKind::PullRequest
                    } else {
                        ItemKind::Issue
                    }),
                },
                by,
            }
        }
        "commit_ref" => TimelineEvent::Other {
            kind: "referenced in a commit".into(),
            by,
        },
        _ => TimelineEvent::Other {
            kind: kind.replace('_', " "),
            by,
        },
    })
}

fn workflow_from_path(path: &str) -> Workflow {
    // `ci.yml@refs/heads/main` (runs) or `.gitea/workflows/ci.yml` (workflows).
    let file = path.split('@').next().unwrap_or_default();
    let name = file.rsplit('/').next().unwrap_or(file);
    Workflow {
        id: ItemId::new(name),
        name: name
            .trim_end_matches(".yml")
            .trim_end_matches(".yaml")
            .to_owned(),
        path: file.to_owned(),
        state: String::new(),
        dispatchable: None,
    }
}

fn workflow(value: &Value) -> Workflow {
    Workflow {
        id: id_of(value, "id"),
        name: optional(value, "name").unwrap_or_else(|| text(value, "id")),
        path: text(value, "path"),
        state: text(value, "state"),
        dispatchable: None,
    }
}

/// A run in Gitea's GitHub-shaped form or Forgejo's own (`index_in_repo`, `status` = result).
pub fn run(value: &Value) -> Run {
    if value.get("index_in_repo").is_some() {
        let word = text(value, "status");
        let (status, conclusion) = result_word(&word);
        let reference = text(value, "prettyref");
        let pr_number = reference
            .strip_prefix('#')
            .and_then(|number| number.parse().ok());
        return Run {
            id: id_of(value, "id"),
            number: count(value, "index_in_repo"),
            title: text(value, "title"),
            workflow: optional(value, "workflow_id").map(|id| workflow_from_path(&id)),
            status,
            conclusion,
            conclusion_raw: conclusion.map(|_| word.to_ascii_lowercase()),
            branch: if pr_number.is_some() {
                String::new()
            } else {
                reference
            },
            sha: text(value, "commit_sha"),
            event: optional(value, "trigger_event").unwrap_or_else(|| text(value, "event")),
            // When it started (a re-run starts long after it was created).
            created: optional_time(value, "started")
                .or_else(|| optional_time(value, "created"))
                .unwrap_or_default(),
            duration: value["duration"]
                .as_u64()
                .filter(|nanos| *nanos > 0)
                .map(std::time::Duration::from_nanos),
            attempt: 1,
            attempts: 1,
            web_url: text(value, "html_url"),
            pr_number,
            needs_approval: value["need_approval"].as_bool() == Some(true),
            actor: run_actor(value),
        };
    }
    let conclusion = optional(value, "conclusion");
    let attempt = value["run_attempt"].as_u64().unwrap_or(1).max(1) as u32;
    Run {
        id: id_of(value, "id"),
        number: count(value, "run_number"),
        title: optional(value, "display_title").unwrap_or_else(|| text(value, "name")),
        workflow: optional(value, "path")
            .or_else(|| optional(value, "workflow_id"))
            .map(|path| workflow_from_path(&path)),
        status: github_status(&text(value, "status")),
        conclusion: conclusion.as_deref().map(conclusion_state),
        conclusion_raw: conclusion,
        branch: text(value, "head_branch"),
        sha: text(value, "head_sha"),
        event: text(value, "event"),
        // When it started (a re-run starts long after it was created).
        created: optional_time(value, "run_started_at")
            .or_else(|| optional_time(value, "started_at"))
            .or_else(|| optional_time(value, "created_at"))
            .unwrap_or_default(),
        duration: (value["status"] == "completed")
            .then(|| span(value, "started_at", "completed_at"))
            .flatten(),
        attempt,
        attempts: attempt,
        web_url: text(value, "html_url"),
        pr_number: value["pull_requests"][0]["number"].as_u64(),
        needs_approval: value["conclusion"] == "action_required",
        actor: run_actor(value),
    }
}

fn step(value: &Value) -> Step {
    if value.get("conclusion").is_some() || value.get("started_at").is_some() {
        let conclusion = optional(value, "conclusion");
        return Step {
            number: count(value, "number"),
            name: text(value, "name"),
            status: github_status(&text(value, "status")),
            conclusion: conclusion.as_deref().map(conclusion_state),
            conclusion_raw: conclusion,
            started: optional_time(value, "started_at"),
            completed: optional_time(value, "completed_at"),
        };
    }
    let word = text(value, "status");
    let (status, conclusion) = result_word(&word);
    Step {
        number: count(value, "number"),
        name: text(value, "name"),
        status,
        conclusion,
        conclusion_raw: conclusion.map(|_| word.to_ascii_lowercase()),
        started: optional_time(value, "started"),
        completed: optional_time(value, "stopped"),
    }
}

/// A job in Gitea's GitHub-shaped form or Forgejo's (`handle`, `task_id`, `status` = result).
pub fn job(value: &Value) -> Job {
    let steps = value["steps"]
        .as_array()
        .map(|steps| steps.iter().map(step).collect());
    if value.get("conclusion").is_some() || value.get("started_at").is_some() {
        let conclusion = optional(value, "conclusion");
        return Job {
            id: id_of(value, "id"),
            name: text(value, "name"),
            status: github_status(&text(value, "status")),
            conclusion: conclusion.as_deref().map(conclusion_state),
            conclusion_raw: conclusion,
            started: optional_time(value, "started_at"),
            completed: optional_time(value, "completed_at"),
            steps: Steps::Known(steps.unwrap_or_default()),
            web_url: text(value, "html_url"),
            stage: None,
            retried: value["run_attempt"]
                .as_u64()
                .is_some_and(|attempt| attempt > 1),
        };
    }
    let word = text(value, "status");
    let (status, conclusion) = result_word(&word);
    Job {
        id: id_of(value, "id"),
        name: text(value, "name"),
        status,
        conclusion,
        conclusion_raw: conclusion.map(|_| word.to_ascii_lowercase()),
        started: None,
        completed: None,
        steps: match steps {
            Some(steps) => Steps::Known(steps),
            None => Steps::Unavailable("steps show on the job itself (Forgejo 17)".into()),
        },
        web_url: text(value, "html_url"),
        stage: None,
        retried: value["attempt"].as_u64().is_some_and(|attempt| attempt > 1),
    }
}

/// `actions/tasks` rows are jobs; runs are their `run_number` groups. Logs are keyed by job,
/// not task, so these jobs have none.
fn task_job(value: &Value) -> Job {
    let word = text(value, "status");
    let (status, conclusion) = result_word(&word);
    Job {
        id: ItemId::new(format!("task-{}", text_or_number(value, "id"))),
        name: text(value, "name"),
        status,
        conclusion,
        conclusion_raw: conclusion.map(|_| word.to_ascii_lowercase()),
        started: optional_time(value, "run_started_at"),
        completed: (status == RunStatus::Completed)
            .then(|| optional_time(value, "updated_at"))
            .flatten(),
        steps: Steps::Unavailable("this server lists tasks, not job steps".into()),
        web_url: text(value, "url"),
        stage: None,
        retried: false,
    }
}

fn text_or_number(value: &Value, key: &str) -> String {
    id_of(value, key).0
}

/// Group tasks (newest first) into runs keyed by `run_number`; the run's id is that number.
pub fn task_runs(tasks: &[Value]) -> Vec<(Run, Vec<Job>)> {
    let mut runs: Vec<(Run, Vec<Job>)> = Vec::new();
    for task in tasks {
        let number = count(task, "run_number");
        let job = task_job(task);
        match runs.iter_mut().find(|(run, _)| run.number == number) {
            Some((run, jobs)) => {
                if let Some(started) = optional_time(task, "run_started_at")
                    && started < run.created
                {
                    run.created = started;
                }
                jobs.push(job);
            }
            None => runs.push((
                Run {
                    id: ItemId::new(number),
                    number,
                    title: optional(task, "display_title").unwrap_or_else(|| text(task, "name")),
                    workflow: optional(task, "workflow_id").map(|id| workflow_from_path(&id)),
                    status: RunStatus::Queued,
                    conclusion: None,
                    conclusion_raw: None,
                    branch: text(task, "head_branch"),
                    sha: text(task, "head_sha"),
                    event: text(task, "event"),
                    created: optional_time(task, "run_started_at")
                        .or_else(|| optional_time(task, "created_at"))
                        .unwrap_or_default(),
                    duration: None,
                    attempt: 1,
                    attempts: 1,
                    web_url: text(task, "url"),
                    pr_number: None,
                    needs_approval: false,
                    actor: String::new(),
                },
                vec![job],
            )),
        }
    }
    for (run, jobs) in &mut runs {
        let any = |state| jobs.iter().any(|job| job.conclusion == Some(state));
        if jobs.iter().any(|job| job.status != RunStatus::Completed) {
            run.status = if jobs.iter().any(|job| job.status == RunStatus::InProgress) {
                RunStatus::InProgress
            } else {
                RunStatus::Queued
            };
        } else {
            run.status = RunStatus::Completed;
            let (state, word) = if any(CiState::Failed) {
                (CiState::Failed, "failure")
            } else if any(CiState::Cancelled) {
                (CiState::Cancelled, "cancelled")
            } else if jobs
                .iter()
                .all(|job| job.conclusion == Some(CiState::Skipped))
            {
                (CiState::Skipped, "skipped")
            } else {
                (CiState::Passed, "success")
            };
            run.conclusion = Some(state);
            run.conclusion_raw = Some(word.into());
        }
    }
    runs
}

fn artifact(value: &Value) -> Artifact {
    Artifact {
        id: id_of(value, "id"),
        name: text(value, "name"),
        size: count(value, "size_in_bytes"),
        expires: optional_time(value, "expires_at"),
        download: optional(value, "archive_download_url")
            .map(ArtifactSource::Url)
            .unwrap_or(ArtifactSource::Cli),
    }
}

fn notification(value: &Value) -> Notification {
    let subject = &value["subject"];
    let url = text(subject, "url");
    let number = url
        .rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok());
    let target = match (subject["type"].as_str(), number) {
        (Some("Pull"), Some(number)) => NotifyTarget::Item {
            kind: ItemKind::PullRequest,
            number,
        },
        (Some("Issue"), Some(number)) => NotifyTarget::Item {
            kind: ItemKind::Issue,
            number,
        },
        (kind, _) => NotifyTarget::Other(kind.unwrap_or_default().into()),
    };
    Notification {
        id: id_of(value, "id"),
        title: text(subject, "title"),
        // Gitea does not say why a thread notified; every thread is a subscription.
        reason: NotifyReason::Subscribed,
        unread: value["unread"].as_bool().unwrap_or(false),
        repo: text(&value["repository"], "full_name"),
        target,
        updated: timestamp(value, "updated_at"),
        comment: optional(subject, "latest_comment_url")
            .and_then(|url| url.rsplit('/').next().map(ItemId::new)),
    }
}

fn merge_style(word: &str) -> Option<MergeMethod> {
    Some(match word {
        "merge" => MergeMethod::Merge,
        "rebase" => MergeMethod::Rebase,
        "rebase-merge" => MergeMethod::RebaseMerge,
        "squash" => MergeMethod::Squash,
        "fast-forward-only" => MergeMethod::FastForwardOnly,
        _ => return None,
    })
}

fn merge_word(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "merge",
        MergeMethod::Rebase => "rebase",
        MergeMethod::RebaseMerge => "rebase-merge",
        MergeMethod::Squash => "squash",
        MergeMethod::FastForwardOnly => "fast-forward-only",
    }
}

fn repo_payload(value: &Value) -> Repo {
    let permissions = &value["permissions"];
    let mut merge_methods = Vec::new();
    for (key, method) in [
        ("allow_merge_commits", MergeMethod::Merge),
        ("allow_rebase", MergeMethod::Rebase),
        ("allow_rebase_explicit", MergeMethod::RebaseMerge),
        ("allow_squash_merge", MergeMethod::Squash),
        (
            "allow_fast_forward_only_merge",
            MergeMethod::FastForwardOnly,
        ),
    ] {
        if value[key].as_bool() == Some(true) {
            merge_methods.push(method);
        }
    }
    Repo {
        id: id_of(value, "id"),
        full_name: text(value, "full_name"),
        default_branch: text(value, "default_branch"),
        web_url: text(value, "html_url"),
        is_fork: value["fork"].as_bool().unwrap_or(false),
        parent: optional(&value["parent"], "full_name"),
        parent_default_branch: optional(&value["parent"], "default_branch"),
        permissions: Permissions {
            admin: permissions["admin"].as_bool().unwrap_or(false),
            push: permissions["push"].as_bool().unwrap_or(false),
            pull: permissions["pull"].as_bool().unwrap_or(true),
        },
        merge_methods,
        default_merge_method: merge_style(&text(value, "default_merge_style")),
        delete_branch_on_merge: value["default_delete_branch_after_merge"]
            .as_bool()
            .unwrap_or(false),
        has_ci: value["has_actions"].as_bool().unwrap_or(true),
        pr_template: None,
    }
}

// ------------------------------------------------------------------------------ queries

/// GitHub search syntax as Gitea issue-list parameters. Qualifiers Gitea has no
/// parameter for become keywords (`q`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListQuery {
    pub params: Vec<(String, String)>,
    pub keywords: Vec<String>,
    /// `repo:o/r` in a host-wide search; rows outside it are dropped.
    pub repo: Option<String>,
    pub state: Option<StateFilter>,
    /// `@me` in a repository list: the caller substitutes the viewer's login.
    pub needs_viewer: bool,
    /// Qualifiers Gitea has no parameter for (`no:label`, `-label:bug`, `no:assignee`,
    /// `no:milestone`, `draft:true`), applied to each row the list returns.
    pub client: Vec<String>,
    /// `sort:` (Gitea's lists have no order parameter): the returned rows are sorted.
    pub sort: Option<String>,
}

/// Whether a row passes the qualifiers Gitea could not take (`ListQuery::client`).
pub fn client_keeps(terms: &[String], value: &Value, row: &ItemRow) -> bool {
    terms.iter().all(|term| {
        let (negated, term) = match term.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, term.as_str()),
        };
        let verdict = match term.split_once(':') {
            Some(("no", "label")) => row.labels.is_empty(),
            Some(("no", "assignee")) => {
                value["assignees"].as_array().is_none_or(Vec::is_empty)
                    && value["assignee"].is_null()
            }
            Some(("no", "milestone")) => value["milestone"].is_null(),
            Some(("label", name)) => {
                let name = name.trim_matches('"');
                row.labels
                    .iter()
                    .any(|label| label.name.eq_ignore_ascii_case(name))
            }
            Some(("draft", value)) => row.draft == (value != "false"),
            Some(("is", "draft")) => row.draft,
            _ => true,
        };
        verdict != negated
    })
}

pub fn translate(filter: &Filter, host_wide: bool) -> ListQuery {
    let mut query = ListQuery::default();
    let mut labels = Vec::new();
    for term in filter.query.split_whitespace() {
        let Some((key, value)) = term.split_once(':') else {
            query.keywords.push(term.to_owned());
            continue;
        };
        let value = value.trim_matches('"');
        let me = value == "@me";
        let mut param = |name: &str, value: &str| query.params.push((name.into(), value.into()));
        match (key, host_wide) {
            ("is", _) => match value {
                "open" => query.state = Some(StateFilter::Open),
                "closed" => query.state = Some(StateFilter::Closed),
                "merged" => query.state = Some(StateFilter::Merged),
                "draft" => query.client.push(term.to_owned()),
                _ => {}
            },
            ("sort", _) => query.sort = Some(value.to_owned()),
            ("archived", _) => {}
            ("draft", _) | ("no", _) | ("-label", _) => query.client.push(term.to_owned()),
            ("label", _) => labels.push(value.to_owned()),
            ("milestone", false) => param("milestones", value),
            ("author", true) if me => param("created", "true"),
            ("assignee", true) if me => param("assigned", "true"),
            ("mentions", true) if me => param("mentioned", "true"),
            ("review-requested", true) if me => param("review_requested", "true"),
            ("reviewed-by", true) if me => param("reviewed", "true"),
            ("user" | "org" | "owner", true) => param("owner", value),
            ("repo", true) => {
                if let Some((owner, _)) = value.split_once('/') {
                    param("owner", owner);
                }
                query.repo = Some(value.to_owned());
            }
            ("author", false) => {
                query.needs_viewer |= me;
                param("created_by", value);
            }
            ("assignee", false) => {
                query.needs_viewer |= me;
                param("assigned_by", value);
            }
            ("mentions", false) => {
                query.needs_viewer |= me;
                param("mentioned_by", value);
            }
            _ => query.keywords.push(term.to_owned()),
        }
    }
    if !labels.is_empty() {
        query.params.push(("labels".into(), labels.join(",")));
    }
    query
}

fn state_word(state: StateFilter) -> &'static str {
    match state {
        StateFilter::Open => "open",
        StateFilter::Closed | StateFilter::Merged => "closed",
        StateFilter::All => "all",
    }
}

fn state_keeps(state: StateFilter, row: &ItemRow) -> bool {
    match state {
        StateFilter::Merged => row.lifecycle == Lifecycle::Merged,
        StateFilter::Closed if row.kind == Some(ItemKind::PullRequest) => {
            row.lifecycle != Lifecycle::Merged
        }
        _ => true,
    }
}

fn total(response: &ApiResponse) -> Option<u64> {
    response.header("x-total-count")?.trim().parse().ok()
}

fn list_items(value: Value, key: Option<&str>) -> Vec<Value> {
    match (value, key) {
        (Value::Array(items), _) => items,
        (mut value, Some(key)) => match value[key].take() {
            Value::Array(items) => items,
            _ => Vec::new(),
        },
        (_, None) => Vec::new(),
    }
}

// ------------------------------------------------------------------------------ viewed files

/// `<state dir>/viewed/<host>/<owner>/<repo>/<n>.json` (next to `hosts.json`).
fn viewed_path(ctx: &ForgeContext, number: u64) -> Result<PathBuf, ForgeError> {
    let repo = ctx.repo()?;
    let state = crate::detect::hosts_path(&crate::credential::Env::process())
        .and_then(|path| path.parent().map(PathBuf::from))
        .ok_or_else(|| ForgeError::Failed("no state directory for viewed files".into()))?;
    Ok(state
        .join("viewed")
        .join(path_segment(&ctx.host().to_string()))
        .join(path_segment(&repo.owner))
        .join(path_segment(&repo.name))
        .join(format!("{number}.json")))
}

fn load_viewed(ctx: &ForgeContext, number: u64) -> BTreeMap<String, bool> {
    viewed_path(ctx, number)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value["viewed"].as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|path| path.as_str().map(|path| (path.to_owned(), true)))
        .collect()
}

fn save_viewed(
    ctx: &ForgeContext,
    number: u64,
    path: &str,
    viewed: bool,
) -> Result<(), ForgeError> {
    let file = viewed_path(ctx, number)?;
    let mut paths = load_viewed(ctx, number);
    if viewed {
        paths.insert(path.to_owned(), true);
    } else {
        paths.remove(path);
    }
    let failed = |error: std::io::Error| ForgeError::Failed(format!("viewed files: {error}"));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let value = json!({ "viewed": paths.keys().collect::<Vec<_>>() });
    let temporary = file.with_extension("json.tmp");
    std::fs::write(&temporary, value.to_string()).map_err(failed)?;
    std::fs::rename(&temporary, &file).map_err(failed)
}

// ------------------------------------------------------------------------------ provider

impl Gitea {
    fn rest(ctx: &ForgeContext, tail: &str) -> Result<ApiRequest, ForgeError> {
        repos_path(ctx, tail).map(ApiRequest::get)
    }

    fn cancelled(cancel: &AtomicBool) -> Result<(), ForgeError> {
        if cancel.load(Ordering::Acquire) {
            Err(ForgeError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Every page of a list, bounded: `page`/`limit` until `X-Total-Count` is reached (or a
    /// short page when the server sends no total).
    fn all_pages(
        ctx: &ForgeContext,
        request: ApiRequest,
        key: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, ForgeError> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            Self::cancelled(cancel)?;
            let response = ctx.ok(
                &request.clone().query("page", page).query("limit", PAGE),
                cancel,
            )?;
            let total = total(&response);
            let batch = list_items(response.json()?, key);
            let size = batch.len();
            all.extend(batch);
            let done = match total {
                Some(total) => all.len() as u64 >= total,
                None => size < PAGE as usize,
            };
            if size == 0 || done {
                break;
            }
        }
        Ok(all)
    }

    /// The rows of one UI page. `per_page` above Gitea's page size is served from consecutive
    /// pages; `keep` drops rows the API cannot filter (merged vs closed).
    fn window(
        ctx: &ForgeContext,
        request: ApiRequest,
        page: Page,
        filtered: bool,
        keep: impl Fn(&Value, &ItemRow) -> bool,
        cancel: &AtomicBool,
    ) -> Result<PageOf<ItemRow>, ForgeError> {
        let per_page = page.per_page.max(1) as usize;
        let chunk = per_page.min(PAGE as usize);
        let skip = (page.number.max(1) as usize - 1) * per_page;
        let first = (skip / chunk) as u32 + 1;
        let offset = skip % chunk;
        let mut rows = Vec::new();
        let mut total = None;
        let mut seen = 0u64;
        let mut more = false;
        for number in first..first + MAX_PAGES {
            Self::cancelled(cancel)?;
            let response = ctx.ok(
                &request.clone().query("page", number).query("limit", chunk),
                cancel,
            )?;
            total = total.or(self::total(&response));
            let batch = list_items(response.json()?, None);
            let size = batch.len();
            seen += size as u64;
            rows.extend(batch.iter().filter_map(|value| {
                let row = row(value);
                keep(value, &row).then_some(row)
            }));
            more = size > 0
                && match total {
                    Some(total) => (skip as u64 - offset as u64) + seen < total,
                    None => size >= chunk,
                };
            if size == 0 || !more || rows.len() >= offset + per_page {
                break;
            }
        }
        let items = rows.into_iter().skip(offset).take(per_page).collect();
        Ok(PageOf {
            items,
            next: more.then_some(Page {
                number: page.number + 1,
                per_page: page.per_page,
            }),
            total: if filtered { None } else { total },
            other_total: None,
        })
    }

    fn viewer_login(ctx: &ForgeContext, cancel: &AtomicBool) -> Result<String, ForgeError> {
        if let Some(viewer) = &ctx.viewer {
            return Ok(viewer.login.clone());
        }
        if ctx.anonymous() {
            return Err(ctx.sign_in_to("filter by @me"));
        }
        let login = user(&ctx.json(&ApiRequest::get("user"), cancel)?).login;
        if login.is_empty() {
            return Err(ForgeError::BadResponse("the viewer has no login".into()));
        }
        Ok(login)
    }

    /// The request a repository list starts from (before `page`/`limit`).
    fn list_request(
        ctx: &ForgeContext,
        kind: ItemKind,
        filter: &Filter,
        viewer: Option<&str>,
    ) -> Result<(ApiRequest, StateFilter), ForgeError> {
        let query = translate(filter, false);
        let state = query.state.unwrap_or(filter.state);
        let simple = query.params.is_empty() && query.keywords.is_empty();
        let mut request = if kind == ItemKind::PullRequest && simple {
            // `pulls` carries head refs and draft flags the picker and rows show.
            Self::rest(ctx, "pulls")?.query("state", state_word(state))
        } else {
            Self::rest(ctx, "issues")?
                .query(
                    "type",
                    if kind == ItemKind::PullRequest {
                        "pulls"
                    } else {
                        "issues"
                    },
                )
                .query("state", state_word(state))
        };
        for (key, value) in &query.params {
            let value = match (value.as_str(), viewer) {
                ("@me", Some(viewer)) => viewer,
                _ => value.as_str(),
            };
            request = request.query(key, value);
        }
        if !query.keywords.is_empty() {
            request = request.query("q", query.keywords.join(" "));
        }
        Ok((request, state))
    }

    fn search_request(kind: ItemKind, filter: &Filter) -> (ApiRequest, StateFilter, ListQuery) {
        let query = translate(filter, true);
        let state = query.state.unwrap_or(filter.state);
        let mut request = ApiRequest::get("repos/issues/search")
            .query(
                "type",
                if kind == ItemKind::PullRequest {
                    "pulls"
                } else {
                    "issues"
                },
            )
            .query("state", state_word(state));
        for (key, value) in &query.params {
            request = request.query(key, value);
        }
        if !query.keywords.is_empty() {
            request = request.query("q", query.keywords.join(" "));
        }
        (request, state, query)
    }

    /// The first requests each Read sends (for plans and `describe()`).
    fn requests(ctx: &ForgeContext, read: &Read) -> Result<Vec<ApiRequest>, ForgeError> {
        let rest = |tail: &str| Self::rest(ctx, tail);
        let first = |request: ApiRequest| request.query("page", 1).query("limit", PAGE);
        Ok(match read {
            Read::Repository => vec![rest("")?],
            Read::RepositoryNamed(name) => vec![ApiRequest::get(format!("repos/{name}"))],
            Read::Viewer => vec![ApiRequest::get("user")],
            Read::Labels => vec![first(rest("labels")?)],
            Read::Milestones => vec![first(rest("milestones")?.query("state", "all"))],
            Read::Collaborators => vec![first(rest("assignees")?)],
            Read::List { kind, filter, .. } => {
                vec![first(Self::list_request(ctx, *kind, filter, None)?.0)]
            }
            Read::Search { kind, filter, .. } => vec![first(Self::search_request(*kind, filter).0)],
            Read::PullRequest(number) => vec![
                ApiRequest::get("version"),
                rest(&format!("pulls/{number}"))?,
                first(rest(&format!("pulls/{number}/files"))?),
                first(rest(&format!("pulls/{number}/commits"))?),
                first(rest(&format!("pulls/{number}/reviews"))?),
            ],
            Read::Issue(number) => vec![rest(&format!("issues/{number}"))?],
            Read::Fingerprint(number) => vec![rest(&format!("pulls/{number}"))?],
            Read::Timeline(number) => vec![first(rest(&format!("issues/{number}/timeline"))?)],
            Read::Threads(number) => vec![first(rest(&format!("pulls/{number}/reviews"))?)],
            Read::Diff(number) => {
                vec![rest(&format!("pulls/{number}.diff"))?.accept(Accept::Diff)]
            }
            Read::CommitDiff(sha) => {
                vec![rest(&format!("git/commits/{sha}.diff"))?.accept(Accept::Diff)]
            }
            Read::Viewed(_) | Read::PullRequestTemplate => Vec::new(),
            Read::PullRequestForBranch(_) => vec![rest("")?],
            Read::Workflows => vec![ApiRequest::get("version"), rest("actions/workflows")?],
            Read::WorkflowSource { id, .. } => {
                vec![rest(&format!("actions/workflows/{}", path_segment(&id.0)))?]
            }
            Read::Runs { page, .. } => vec![
                ApiRequest::get("version"),
                rest("actions/runs")?
                    .query("page", page.number.max(1))
                    .query("limit", page.per_page.clamp(1, PAGE)),
            ],
            Read::Run { id, .. } => vec![
                ApiRequest::get("version"),
                rest(&format!("actions/runs/{id}"))?,
                rest(&format!("actions/runs/{id}/jobs"))?,
            ],
            Read::Job(id) => vec![rest(&format!("actions/jobs/{id}"))?],
            Read::JobLog(id) => vec![
                rest(&format!("actions/jobs/{id}/logs"))?
                    .accept(Accept::Text)
                    .timeout(crate::transport::LOG_TIMEOUT),
            ],
            Read::Artifacts(run) => vec![rest(&format!("actions/runs/{run}/artifacts"))?],
            Read::ArtifactFiles(id) | Read::ArtifactFile { artifact: id, .. } => {
                vec![super::download(rest(&format!(
                    "actions/artifacts/{id}/zip"
                ))?)]
            }
            Read::PullRequests(numbers) => numbers
                .iter()
                .map(|number| rest(&format!("pulls/{number}")))
                .collect::<Result<_, _>>()?,
            Read::Fingerprints(numbers) => numbers
                .iter()
                .map(|number| rest(&format!("pulls/{number}")))
                .collect::<Result<_, _>>()?,
            Read::Notifications { filter, page } => {
                vec![Self::notifications_request(filter, *page)]
            }
        })
    }

    fn notifications_request(filter: &NotifyFilter, page: Page) -> ApiRequest {
        let mut request = ApiRequest::get("notifications").class(Class::Background);
        request = if filter.all {
            request.query("all", "true")
        } else {
            request.query("status-types", "unread")
        };
        if let Some(since) = &filter.since {
            request = request.query("since", since.as_str());
        }
        request
            .query("page", page.number.max(1))
            .query("limit", page.per_page.clamp(1, PAGE))
    }

    /// Send a probed endpoint: on a 404 from a server whose version did not decide, remember
    /// it (F2 `hosts.json`) and let the caller fall back.
    fn probe(
        ctx: &ForgeContext,
        request: &ApiRequest,
        known: bool,
        capability: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<ApiResponse>, ForgeError> {
        // An endpoint a version-less server already answered 404 for is not asked again: the
        // fallback is used straight away (the observation lives in hosts.json).
        if !known
            && crate::detect::observed_capabilities(ctx.host())
                .get(capability)
                .is_some_and(|seen| seen == "no")
        {
            return Ok(None);
        }
        let response = ctx.send(request, cancel)?;
        // Anonymous: some instances (gitea.com) keep `actions/runs` for signed-in users while
        // `actions/tasks` stays public; use the fallback without recording anything.
        if response.status == 401 && ctx.anonymous() {
            return Ok(None);
        }
        if response.status == 404 && !known {
            crate::detect::record_capability(ctx.host(), &ctx.endpoints, capability, false);
            return Ok(None);
        }
        if !response.success() {
            return Err(crate::http::classify(&ctx.endpoints, &response));
        }
        if !known {
            crate::detect::record_capability(ctx.host(), &ctx.endpoints, capability, true);
        }
        Ok(Some(response))
    }

    /// Reads without a credential: public repositories answer the REST reads
    /// anonymously, except the viewer, notifications, assignees and (on most instances)
    /// Actions logs. Those answer with what a sign-in would unlock instead of a host-wide
    /// "signed out".
    fn anonymous(
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        match read {
            Read::Viewer => return Err(ctx.sign_in_to("see your account")),
            Read::Notifications { .. } => return Err(ctx.sign_in_to("see notifications")),
            _ => {}
        }
        match Self::fetch(ctx, read, cond, cancel) {
            Err(ForgeError::NotAuthenticated { .. }) => match read {
                Read::Collaborators => Ok(Fetched::payload(Payload::Users(Vec::new()))),
                Read::JobLog(_) => Err(ctx.sign_in_to("read logs")),
                Read::Workflows | Read::WorkflowSource { .. } | Read::Runs { .. } => {
                    Err(ctx.sign_in_to("see Actions"))
                }
                Read::Run { .. } | Read::Job(_) | Read::Artifacts(_) => {
                    Err(ctx.sign_in_to("see this run"))
                }
                Read::ArtifactFiles(_) | Read::ArtifactFile { .. } => {
                    Err(ctx.sign_in_to("download artifacts"))
                }
                _ => Err(ctx.sign_in_to("read this")),
            },
            result => result,
        }
    }

    fn statuses(
        ctx: &ForgeContext,
        sha: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<Check>, ForgeError> {
        if sha.is_empty() {
            return Ok(Vec::new());
        }
        // The combined status keeps the newest status per context.
        let value = ctx.json(&Self::rest(ctx, &format!("commits/{sha}/status"))?, cancel)?;
        Ok(value["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|status| status_check(ctx, status))
            .collect())
    }

    /// A status links its run by the run's index in the repository (`…/actions/runs/12`),
    /// while the API reads a run by its id: list the head commit's runs once and swap each
    /// index for that run's id. A server without the runs list keeps the index, which its
    /// `actions/tasks` fallback reads runs by; a run the list does not hold loses its link
    /// (the check opens the forge's page instead).
    fn run_ids(ctx: &ForgeContext, sha: &str, checks: &mut [Check], cancel: &AtomicBool) {
        if sha.is_empty() || checks.iter().all(|check| check.run_link.is_none()) {
            return;
        }
        if server(ctx, cancel).require(&RUNS).is_err() {
            return;
        }
        let Ok(request) = Self::rest(ctx, "actions/runs") else {
            return;
        };
        // Gitea filters by `head_sha`; a server that ignores it still lists the newest runs.
        let request = request.query("head_sha", sha).query("limit", PAGE);
        let Ok(value) = ctx.json(&request, cancel) else {
            return;
        };
        let runs = list_items(value, Some("workflow_runs"));
        for check in checks.iter_mut() {
            let Some(link) = check.run_link.as_mut() else {
                continue;
            };
            let found = runs
                .iter()
                // Gitea's GitHub-shaped `run_number`, Forgejo's own `index_in_repo`.
                .find(|run| {
                    run["run_number"]
                        .as_u64()
                        .or_else(|| run["index_in_repo"].as_u64())
                        .is_some_and(|number| number.to_string() == link.run.0)
                })
                .map(|run| id_of(run, "id"));
            match found {
                Some(id) => link.run = id,
                // An index is no id: read as one it names another run, or none. The check
                // keeps its link to the forge's own page.
                None => check.run_link = None,
            }
        }
    }

    /// The issues the description closes (`Closes #12`): neither forge lists them for a pull
    /// request. This repository's are read for their titles (the first few; one that cannot
    /// be read keeps its number), another repository's keep their name.
    fn closing_issues(ctx: &ForgeContext, pull: &Value, cancel: &AtomicBool) -> Vec<LinkRef> {
        const TITLED: usize = 5;
        let repo = text(&pull["base"]["repo"], "full_name");
        let own_web = text(&pull["base"]["repo"], "html_url");
        super::closing::closing_references(&text(pull, "body"), &repo)
            .into_iter()
            .enumerate()
            .filter_map(|(index, closing)| {
                let mut link = LinkRef {
                    number: closing.number,
                    title: String::new(),
                    web_url: None,
                    repo: closing.repo.clone(),
                    kind: Some(ItemKind::Issue),
                };
                match &closing.repo {
                    Some(other) => {
                        link.web_url =
                            Some(web(ctx, &format!("{other}/issues/{}", closing.number)));
                    }
                    None => {
                        if !own_web.is_empty() {
                            link.web_url = Some(format!(
                                "{}/issues/{}",
                                own_web.trim_end_matches('/'),
                                closing.number
                            ));
                        }
                        if index < TITLED
                            && let Ok(request) =
                                Self::rest(ctx, &format!("issues/{}", closing.number))
                            && let Ok(issue) = ctx.json(&request, cancel)
                        {
                            // `Closes #4` naming a pull request closes nothing.
                            if issue["pull_request"].is_object() {
                                return None;
                            }
                            link.title = text(&issue, "title");
                            if let Some(url) = optional(&issue, "html_url") {
                                link.web_url = Some(url);
                            }
                        }
                    }
                }
                Some(link)
            })
            .collect()
    }

    fn pull_request(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<PullRequest, ForgeError> {
        let pull = ctx.json(&Self::rest(ctx, &format!("pulls/{number}"))?, cancel)?;
        let viewed = load_viewed(ctx, number);
        let head = ref_info(&pull["head"]);
        let [files, commits, reviews] = ["files", "commits", "reviews"]
            .map(|part| Self::rest(ctx, &format!("pulls/{number}/{part}")));
        let (files, commits, reviews) = (files?, commits?, reviews?);
        // Everything else hangs off the pull itself: one more round trip, not five.
        let (files, commits, reviews, checks, linked_issues) = std::thread::scope(|scope| {
            let pages = |request| scope.spawn(move || Self::all_pages(ctx, request, None, cancel));
            let (files, commits, reviews) = (pages(files), pages(commits), pages(reviews));
            let linked = scope.spawn(|| Self::closing_issues(ctx, &pull, cancel));
            let reactions = scope.spawn(|| Self::reactions(ctx, number, cancel));
            let checks = Self::statuses(ctx, &head.sha, cancel).map(|mut checks| {
                Self::run_ids(ctx, &head.sha, &mut checks, cancel);
                checks
            });
            use super::parallel::joined;
            let linked = (joined(linked), joined(reactions));
            (
                joined(files),
                joined(commits),
                joined(reviews),
                checks,
                linked,
            )
        });
        let (linked_issues, reactions) = linked_issues;
        let files = files?
            .iter()
            .map(|file| changed_file(file, &viewed))
            .collect::<Vec<_>>();
        let commits = commits?.iter().map(commit).collect::<Vec<_>>();
        let reviews = reviews?.iter().map(review).collect::<Vec<_>>();
        let checks = checks?;
        let reviewers = reviewer_slots(&pull, &reviews);
        let rollup = checks::rollup(&checks);
        let mut item = row(&pull);
        item.review = decision(&reviewers);
        item.ci = rollup;
        item.ci_counts = ci_counts(&checks);
        item.changed_files = item.changed_files.or(Some(files.len() as u64));
        Ok(PullRequest {
            body: text(&pull, "body"),
            base: ref_info(&pull["base"]),
            pinned_head: head.sha.clone(),
            head,
            mergeable: mergeable(&pull),
            reviewers,
            reviews,
            checks,
            files,
            commits,
            assignees: users(&pull["assignees"]),
            milestone: milestone(&pull["milestone"]),
            linked_issues,
            created: timestamp(&pull, "created_at"),
            fingerprint: fingerprint(&pull, rollup),
            projects: Vec::new(),
            subscribed: None,
            reactions,
            row: item,
        })
    }

    /// The reactions on an issue's or pull request's body (one request; none where the
    /// instance refuses or predates them).
    fn reactions(ctx: &ForgeContext, number: u64, cancel: &AtomicBool) -> Vec<Reaction> {
        Self::rest(ctx, &format!("issues/{number}/reactions"))
            .and_then(|request| ctx.json(&request.query("limit", "50"), cancel))
            .map(|list| reactions::gitea(&list))
            .unwrap_or_default()
    }

    fn review_threads(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<ReviewThread>, ForgeError> {
        let reviews = Self::all_pages(
            ctx,
            Self::rest(ctx, &format!("pulls/{number}/reviews"))?,
            None,
            cancel,
        )?;
        let commented = reviews
            .iter()
            .filter(|review| review["comments_count"].as_u64() != Some(0))
            .map(|review| text_or_number(review, "id"))
            .collect::<Vec<_>>();
        // Each review's comments are their own request; a few go out at once.
        let pages = parallel::map(&commented, |id| -> Result<Vec<Value>, ForgeError> {
            Self::cancelled(cancel)?;
            let request = Self::rest(ctx, &format!("pulls/{number}/reviews/{id}/comments"))?;
            let response = ctx.send(&request, cancel)?;
            // A review deleted since the list was read: its comments went with it.
            if response.status == 404 {
                return Ok(Vec::new());
            }
            if !response.success() {
                return Err(crate::http::classify(&ctx.endpoints, &response));
            }
            Ok(list_items(response.json()?, None))
        });
        let mut comments = Vec::new();
        for page in pages {
            comments.extend(page?);
        }
        Ok(threads(&comments))
    }

    fn timeline(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<TimelineEvent>, ForgeError> {
        let events = Self::all_pages(
            ctx,
            Self::rest(ctx, &format!("issues/{number}/timeline"))?,
            None,
            cancel,
        )?;
        // Review entries carry an id only; their verdicts come from the PR's reviews.
        let mut reviews = HashMap::new();
        if events.iter().any(|event| event["type"] == "review") {
            for value in Self::all_pages(
                ctx,
                Self::rest(ctx, &format!("pulls/{number}/reviews"))?,
                None,
                cancel,
            )? {
                reviews.insert(count(&value, "id"), review(&value));
            }
        }
        Ok(events
            .iter()
            .filter_map(|event| timeline_event(event, &reviews))
            .collect())
    }

    fn pull_for_branch(
        ctx: &ForgeContext,
        branch: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<u64>, ForgeError> {
        let repo = ctx.json(&Self::rest(ctx, "")?, cancel)?;
        let full = text(&repo, "full_name");
        let base = text(&repo, "default_branch");
        let server = server(ctx, cancel);
        let same_repo = |pull: &Value| {
            text(&pull["head"]["repo"], "full_name") == full || pull["head"]["repo"].is_null()
        };
        if server.supports(&PR_FOR_BRANCH) != Some(false) && !base.is_empty() {
            let request = Self::rest(ctx, &format!("pulls/{}/{}", path_segment(&base), branch))?;
            let response = ctx.send(&request, cancel)?;
            if response.success() {
                let pull = response.json()?;
                return Ok((text(&pull, "state") == "open" && same_repo(&pull))
                    .then(|| pull["number"].as_u64())
                    .flatten());
            }
            if response.status != 404 {
                return Err(crate::http::classify(&ctx.endpoints, &response));
            }
        }
        // Old servers, or a PR into another base: scan the open ones (base-repo heads only,
        // like GitHub's `head=owner:branch`).
        let pulls = Self::all_pages(
            ctx,
            Self::rest(ctx, "pulls")?.query("state", "open"),
            None,
            cancel,
        )?;
        Ok(pulls
            .iter()
            .find(|pull| text(&pull["head"], "ref") == branch && same_repo(pull))
            .and_then(|pull| pull["number"].as_u64()))
    }

    fn tasks(
        ctx: &ForgeContext,
        page: u32,
        limit: u32,
        cancel: &AtomicBool,
    ) -> Result<(Vec<Value>, Option<u64>), ForgeError> {
        let server = server(ctx, cancel);
        server.require(&TASKS)?;
        let response = ctx.ok(
            &Self::rest(ctx, "actions/tasks")?
                .query("page", page)
                .query("limit", limit),
            cancel,
        )?;
        let header = total(&response);
        let value = response.json()?;
        let total = value["total_count"].as_u64().or(header);
        Ok((list_items(value, Some("workflow_runs")), total))
    }

    fn runs(
        ctx: &ForgeContext,
        filter: &RunFilter,
        page: Page,
        cancel: &AtomicBool,
    ) -> Result<PageOf<Run>, ForgeError> {
        let server = server(ctx, cancel);
        let limit = page.per_page.clamp(1, PAGE);
        let number = page.number.max(1);
        let workflow = filter.workflow.as_ref().map(crate::legacy::workflow_id);
        let status = filter.status.as_deref().map(|status| match status {
            // `action_required` is Gitea's `blocked` (a fork run waiting for approval).
            "action_required" => "blocked",
            "in_progress" => "running",
            "queued" => "waiting",
            "cancelled" => "cancelled",
            other => other,
        });
        let keep = |run: &Run| {
            workflow.as_ref().is_none_or(|workflow| {
                run.workflow
                    .as_ref()
                    .is_some_and(|known| known.id == *workflow || known.path == workflow.0)
            }) && filter
                .branch
                .as_ref()
                .is_none_or(|branch| run.branch == *branch)
                && filter
                    .event
                    .as_ref()
                    .is_none_or(|event| run.event == *event)
                && match filter.status.as_deref() {
                    None => true,
                    Some("action_required") => run.needs_approval,
                    Some("in_progress" | "queued" | "waiting" | "pending") => {
                        run.status != RunStatus::Completed
                    }
                    Some(word) => run.conclusion_raw.as_deref() == Some(word),
                }
        };
        let next = |total: Option<u64>, size: usize| {
            let more = match total {
                Some(total) => total > (number as u64) * limit as u64,
                None => size >= limit as usize,
            };
            more.then_some(Page {
                number: number + 1,
                per_page: limit,
            })
        };
        if let Ok(known) = server.require(&RUNS) {
            let mut request = Self::rest(ctx, "actions/runs")?
                .query("page", number)
                .query("limit", limit);
            if let Some(branch) = &filter.branch {
                request = request.query("branch", branch);
            }
            if let Some(event) = &filter.event {
                request = request.query("event", event);
            }
            if let Some(status) = status {
                request = request.query("status", status);
            }
            if let Some(response) = Self::probe(ctx, &request, known, "runs_list", cancel)? {
                let header = total(&response);
                let value = response.json()?;
                let total = value["total_count"].as_u64().or(header);
                let all = list_items(value, Some("workflow_runs"));
                let size = all.len();
                let items = all.iter().map(run).filter(keep).collect::<Vec<_>>();
                let filtered = workflow.is_some() || status.is_some();
                return Ok(PageOf {
                    next: next(total, size),
                    items,
                    total: if filtered { None } else { total },
                    other_total: None,
                });
            }
        }
        // `actions/tasks` (Gitea 1.23, Forgejo 8): one row per job, grouped into runs here.
        let (tasks, total) = Self::tasks(ctx, number, limit, cancel)?;
        let size = tasks.len();
        Ok(PageOf {
            items: task_runs(&tasks)
                .into_iter()
                .map(|(run, _)| run)
                .filter(keep)
                .collect(),
            next: next(total, size),
            total: None,
            other_total: None,
        })
    }

    fn failure(jobs: &[Job]) -> Option<FailureLine> {
        jobs.iter()
            .find(|job| job.conclusion == Some(CiState::Failed))
            .map(|job| FailureLine {
                job: job.id.clone(),
                line: format!("{} failed", job.name),
            })
    }

    fn run_detail(
        ctx: &ForgeContext,
        id: &ItemId,
        attempt: Option<u32>,
        cancel: &AtomicBool,
    ) -> Result<RunDetail, ForgeError> {
        let server = server(ctx, cancel);
        // Forgejo serves a run from 12, its jobs from 16; Gitea both from 1.25.
        if let Ok(known) = server.require(&RUNS) {
            let attempt = attempt.filter(|_| server.supports(&ATTEMPTS) == Some(true));
            let path = match attempt {
                Some(attempt) => format!("actions/runs/{id}/attempts/{attempt}"),
                None => format!("actions/runs/{id}"),
            };
            if let Some(response) =
                Self::probe(ctx, &Self::rest(ctx, &path)?, known, "run", cancel)?
            {
                let mut run = run(&response.json()?);
                let jobs = match server.require(&RUN_JOBS) {
                    Ok(known) => {
                        let request = Self::rest(ctx, &format!("actions/runs/{id}/jobs"))?;
                        match Self::probe(ctx, &request, known, "run_jobs", cancel)? {
                            Some(response) => list_items(response.json()?, Some("jobs"))
                                .iter()
                                .map(job)
                                .collect(),
                            None => Self::task_jobs(ctx, run.number, cancel)?,
                        }
                    }
                    Err(_) => Self::task_jobs(ctx, run.number, cancel)?,
                };
                if attempt.is_none() {
                    run.attempts = run.attempt;
                }
                return Ok(RunDetail {
                    failure_summary: Self::failure(&jobs),
                    run,
                    jobs,
                });
            }
        }
        // Tasks only: the run id is its number.
        let number =
            id.0.parse::<u64>()
                .map_err(|_| ForgeError::NotFound(format!("run {id}")))?;
        let (tasks, _) = Self::tasks(ctx, 1, PAGE, cancel)?;
        let (run, jobs) = task_runs(&tasks)
            .into_iter()
            .find(|(run, _)| run.number == number)
            .ok_or_else(|| {
                ForgeError::NotFound(format!("run {id} is not among the recent ones"))
            })?;
        Ok(RunDetail {
            failure_summary: Self::failure(&jobs),
            run,
            jobs,
        })
    }

    fn task_jobs(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<Job>, ForgeError> {
        let (tasks, _) = Self::tasks(ctx, 1, PAGE, cancel)?;
        Ok(task_runs(&tasks)
            .into_iter()
            .find(|(run, _)| run.number == number)
            .map(|(_, jobs)| jobs)
            .unwrap_or_default())
    }

    fn job_log(ctx: &ForgeContext, id: &ItemId, cancel: &AtomicBool) -> Result<Log, ForgeError> {
        if id.0.starts_with("task-") {
            return Err(ForgeError::Unsupported {
                capability: "job_log",
                reason: "this server lists tasks without logs · o opens the run in the browser"
                    .into(),
            });
        }
        server(ctx, cancel).require(&JOB_LOG)?;
        let request = Self::rest(ctx, &format!("actions/jobs/{id}/logs"))?
            .accept(Accept::Text)
            .timeout(crate::transport::LOG_TIMEOUT);
        let cap = request.body_cap;
        let log =
            |text: String, truncated: bool| Log::new(id.clone(), text, LogFormat::Plain, truncated);
        match ctx.send(&request, cancel) {
            Ok(response) if response.success() => Ok(log(response.text(), false)),
            // Forgejo answers a job that never ran with `500 {"message":""}`.
            Ok(response)
                if response.status == 500
                    && response
                        .json()
                        .ok()
                        .is_some_and(|body| text(&body, "message").is_empty()) =>
            {
                Ok(log(NEVER_RAN.into(), false))
            }
            Ok(response) => Err(crate::http::classify(&ctx.endpoints, &response)),
            Err(ForgeError::BadResponse(message)) if message.contains("exceeds") => Ok(log(
                format!(
                    "log exceeds {} MiB · open it in the browser",
                    cap / (1024 * 1024)
                ),
                true,
            )),
            Err(error) => Err(error),
        }
    }

    /// The workflow files of the first workflow directory the repository has, in the order
    /// the server reads them (`.forgejo/` only on Forgejo); one request per directory tried.
    fn workflow_files(
        ctx: &ForgeContext,
        server: Server,
        cancel: &AtomicBool,
    ) -> Result<Vec<Workflow>, ForgeError> {
        let directories: &[&str] = if server.kind() == ProviderKind::Forgejo {
            &[
                ".forgejo/workflows",
                ".gitea/workflows",
                ".github/workflows",
            ]
        } else {
            &[".gitea/workflows", ".github/workflows"]
        };
        for directory in directories {
            Self::cancelled(cancel)?;
            let response = ctx.send(&Self::rest(ctx, &format!("contents/{directory}"))?, cancel)?;
            if !response.success() {
                // Absent (404) or hidden from this account: the next directory, then none.
                continue;
            }
            let files = response
                .json()?
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| text(entry, "type") == "file")
                .map(|entry| text(entry, "path"))
                .filter(|path| path.ends_with(".yml") || path.ends_with(".yaml"))
                .map(|path| workflow_from_path(&path))
                .collect::<Vec<_>>();
            if !files.is_empty() {
                return Ok(files);
            }
        }
        Ok(Vec::new())
    }

    fn workflow_source(
        ctx: &ForgeContext,
        id: &ItemId,
        reference: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<String, ForgeError> {
        let server = server(ctx, cancel);
        let mut candidates = Vec::new();
        if id.0.contains('/') {
            candidates.push(id.0.clone());
        } else {
            if server.supports(&WORKFLOWS) != Some(false) {
                let request =
                    Self::rest(ctx, &format!("actions/workflows/{}", path_segment(&id.0)))?;
                if let Ok(response) = ctx.send(&request, cancel)
                    && response.success()
                    && let Some(path) = optional(&response.json()?, "path")
                {
                    candidates.push(path);
                }
            }
            for directory in [
                ".forgejo/workflows",
                ".gitea/workflows",
                ".github/workflows",
            ] {
                candidates.push(format!("{directory}/{}", id.0));
            }
        }
        for path in candidates {
            Self::cancelled(cancel)?;
            let mut request = Self::rest(ctx, &format!("raw/{path}"))?.accept(Accept::Raw);
            if let Some(reference) = reference.filter(|reference| !reference.is_empty()) {
                request = request.query("ref", reference);
            }
            let response = ctx.send(&request, cancel)?;
            if response.success() {
                return Ok(response.text());
            }
            if response.status != 404 {
                return Err(crate::http::classify(&ctx.endpoints, &response));
            }
        }
        Err(ForgeError::NotFound(format!("workflow {id}")))
    }

    fn fetch(
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        let rest = |tail: &str| Self::rest(ctx, tail);
        let payload = match read {
            Read::Repository | Read::RepositoryNamed(_) => {
                let mut request = match read {
                    Read::RepositoryNamed(name) => ApiRequest::get(format!("repos/{name}")),
                    _ => rest("")?,
                };
                if let Some(etag) = &cond.etag {
                    request = request.header("If-None-Match", etag.clone());
                }
                let response = ctx.send(&request, cancel)?;
                if response.status == 304 {
                    return Ok(Fetched::NotModified);
                }
                if !response.success() {
                    return Err(crate::http::classify(&ctx.endpoints, &response));
                }
                return Ok(Fetched::Payload {
                    value: Payload::Repo(repo_payload(&response.json()?)),
                    etag: response.header("etag").map(str::to_owned),
                    fingerprint: None,
                });
            }
            Read::Viewer => Payload::User(user(&ctx.json(&ApiRequest::get("user"), cancel)?)),
            Read::Labels => Payload::Labels(
                Self::all_pages(ctx, rest("labels")?, None, cancel)?
                    .iter()
                    .map(label)
                    .collect(),
            ),
            Read::Milestones => Payload::Milestones(
                Self::all_pages(ctx, rest("milestones")?.query("state", "all"), None, cancel)?
                    .iter()
                    .filter_map(milestone)
                    .collect(),
            ),
            Read::Collaborators => Payload::Users(users(&Value::Array(Self::all_pages(
                ctx,
                rest("assignees")?,
                None,
                cancel,
            )?))),
            Read::List { kind, filter, page } => {
                let viewer = if translate(filter, false).needs_viewer {
                    Some(Self::viewer_login(ctx, cancel)?)
                } else {
                    None
                };
                let (request, state) = Self::list_request(ctx, *kind, filter, viewer.as_deref())?;
                let query = translate(filter, false);
                let filtered = (matches!(state, StateFilter::Merged | StateFilter::Closed)
                    && *kind == ItemKind::PullRequest)
                    || !query.client.is_empty();
                let mut rows = Self::window(
                    ctx,
                    request,
                    *page,
                    filtered,
                    |value, row| state_keeps(state, row) && client_keeps(&query.client, value, row),
                    cancel,
                )?;
                if let Some(order) = &query.sort {
                    super::common::sort_rows(&mut rows.items, order);
                }
                Payload::Rows(rows)
            }
            Read::Search { kind, filter, page } => {
                let (request, state, query) = Self::search_request(*kind, filter);
                let filtered = query.repo.is_some()
                    || !query.client.is_empty()
                    || (matches!(state, StateFilter::Merged | StateFilter::Closed)
                        && *kind == ItemKind::PullRequest);
                let mut rows = Self::window(
                    ctx,
                    request,
                    *page,
                    filtered,
                    |value, row| {
                        state_keeps(state, row)
                            && client_keeps(&query.client, value, row)
                            && query
                                .repo
                                .as_ref()
                                .is_none_or(|repo| row.repo.as_deref() == Some(repo))
                    },
                    cancel,
                )?;
                if let Some(order) = &query.sort {
                    super::common::sort_rows(&mut rows.items, order);
                }
                Payload::Rows(rows)
            }
            Read::PullRequest(number) => {
                let pull = Self::pull_request(ctx, *number, cancel)?;
                let fingerprint = Some(pull.fingerprint.clone());
                return Ok(Fetched::Payload {
                    value: Payload::PullRequest(Box::new(pull)),
                    etag: None,
                    fingerprint,
                });
            }
            Read::Issue(number) => {
                let (value, reactions) = std::thread::scope(|scope| {
                    let reactions = scope.spawn(|| Self::reactions(ctx, *number, cancel));
                    let value = rest(&format!("issues/{number}"))
                        .and_then(|request| ctx.json(&request, cancel));
                    (value, parallel::joined(reactions))
                });
                let mut issue = issue(&value?);
                issue.reactions = reactions;
                Payload::Issue(Box::new(issue))
            }
            Read::Fingerprint(number) => {
                let pull = ctx.json(&rest(&format!("pulls/{number}"))?, cancel)?;
                let checks = Self::statuses(ctx, &text(&pull["head"], "sha"), cancel)?;
                let fingerprint = fingerprint(&pull, checks::rollup(&checks));
                if cond.fingerprint.as_ref() == Some(&fingerprint) {
                    return Ok(Fetched::NotModified);
                }
                Payload::Fingerprint(fingerprint)
            }
            Read::Timeline(number) => Payload::Timeline(Self::timeline(ctx, *number, cancel)?),
            Read::Threads(number) => Payload::Threads(Self::review_threads(ctx, *number, cancel)?),
            Read::Viewed(number) => {
                Payload::Viewed(load_viewed(ctx, *number).into_iter().collect())
            }
            Read::Diff(number) => Payload::Text(
                ctx.ok(
                    &rest(&format!("pulls/{number}.diff"))?.accept(Accept::Diff),
                    cancel,
                )?
                .text(),
            ),
            Read::CommitDiff(sha) => Payload::Text(
                ctx.ok(
                    &rest(&format!("git/commits/{sha}.diff"))?.accept(Accept::Diff),
                    cancel,
                )?
                .text(),
            ),
            Read::PullRequestForBranch(branch) => {
                Payload::MaybePullRequest(Self::pull_for_branch(ctx, branch, cancel)?)
            }
            Read::Workflows => {
                let server = server(ctx, cancel);
                let listed = match server.require(&WORKFLOWS) {
                    // Forgejo has no list.
                    Err(_) => None,
                    Ok(known) => {
                        Self::probe(ctx, &rest("actions/workflows")?, known, "workflows", cancel)?
                            .map(|response| {
                                Ok::<_, ForgeError>(
                                    list_items(response.json()?, Some("workflows"))
                                        .iter()
                                        .map(workflow)
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .transpose()?
                    }
                };
                // Without the list, the workflow files are the Workflows panel (their on/off
                // state is not in any API); with none, the runs name theirs.
                Payload::Workflows(match listed {
                    Some(workflows) => workflows,
                    None => Self::workflow_files(ctx, server, cancel)?,
                })
            }
            Read::WorkflowSource { id, reference } => Payload::Text(Self::workflow_source(
                ctx,
                &crate::legacy::workflow_id(id),
                reference.as_deref(),
                cancel,
            )?),
            Read::Runs { filter, page } => Payload::Runs(Self::runs(ctx, filter, *page, cancel)?),
            Read::Run { id, attempt } => {
                Payload::Run(Box::new(Self::run_detail(ctx, id, *attempt, cancel)?))
            }
            Read::Job(id) => {
                server(ctx, cancel).require(&JOB)?;
                Payload::Job(Box::new(job(
                    &ctx.json(&rest(&format!("actions/jobs/{id}"))?, cancel)?
                )))
            }
            Read::JobLog(id) => Payload::Log(Self::job_log(ctx, id, cancel)?),
            Read::Artifacts(run) => {
                server(ctx, cancel).require(&ARTIFACTS)?;
                let value = ctx.json(&rest(&format!("actions/runs/{run}/artifacts"))?, cancel)?;
                Payload::Artifacts(
                    list_items(value, Some("artifacts"))
                        .iter()
                        .map(artifact)
                        .collect(),
                )
            }
            Read::ArtifactFiles(id) | Read::ArtifactFile { artifact: id, .. } => {
                server(ctx, cancel).require(&ARTIFACTS)?;
                let request = super::download(rest(&format!("actions/artifacts/{id}/zip"))?);
                let response = ctx.ok(&request, cancel)?;
                super::artifact_payload(read, &response.body)?
            }
            Read::PullRequests(numbers) => {
                let mut pulls = Vec::new();
                for number in numbers {
                    match Self::fetch(
                        ctx,
                        &Read::PullRequest(*number),
                        &Conditional::default(),
                        cancel,
                    ) {
                        Ok(Fetched::Payload {
                            value: Payload::PullRequest(pull),
                            ..
                        }) => pulls.push(*pull),
                        Err(ForgeError::NotFound(_)) | Ok(_) => {}
                        Err(error) => return Err(error),
                    }
                }
                Payload::PullRequests(pulls)
            }
            Read::Fingerprints(numbers) => {
                // The list's check marks ask for every visible row: a few at once.
                let fetched = parallel::map(numbers, |number| {
                    Self::fetch(
                        ctx,
                        &Read::Fingerprint(*number),
                        &Conditional::default(),
                        cancel,
                    )
                });
                let mut prints = Vec::new();
                for (number, fetched) in numbers.iter().zip(fetched) {
                    match fetched {
                        Ok(Fetched::Payload {
                            value: Payload::Fingerprint(print),
                            ..
                        }) => prints.push((*number, print)),
                        Err(ForgeError::NotFound(_)) | Ok(_) => {}
                        Err(error) => return Err(error),
                    }
                }
                Payload::Fingerprints(prints)
            }
            Read::Notifications { filter, page } => {
                let mut request = Self::notifications_request(filter, *page);
                // Incremental refresh: only threads updated since the cached copy.
                if let Some(since) = cond.since.as_ref().filter(|_| filter.since.is_none()) {
                    request = request.query("since", since.as_str());
                }
                let response = ctx.ok(&request, cancel)?;
                let total = total(&response);
                let items = list_items(response.json()?, None);
                let limit = page.per_page.clamp(1, PAGE);
                let more = match total {
                    Some(total) => total > page.number.max(1) as u64 * limit as u64,
                    None => items.len() >= limit as usize,
                };
                Payload::Notifications(PageOf {
                    items: items.iter().map(notification).collect(),
                    next: more.then_some(Page {
                        number: page.number.max(1) + 1,
                        per_page: page.per_page,
                    }),
                    total,
                    other_total: None,
                })
            }
            // Read from the base ref locally, never through the API.
            Read::PullRequestTemplate => {
                Payload::Template(crate::template::for_context(&ctx.root, ctx.endpoints.kind))
            }
        };
        Ok(Fetched::payload(payload))
    }

    // -------------------------------------------------------------------------- writes

    fn label_ids(
        ctx: &ForgeContext,
        names: &[String],
        cancel: &AtomicBool,
    ) -> Result<Vec<u64>, ForgeError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let known = Self::all_pages(ctx, Self::rest(ctx, "labels")?, None, cancel)?;
        names
            .iter()
            .map(|name| {
                name.trim().parse::<u64>().ok().map(Ok).unwrap_or_else(|| {
                    known
                        .iter()
                        .find(|label| text(label, "name").eq_ignore_ascii_case(name.trim()))
                        .and_then(|label| label["id"].as_u64())
                        .ok_or_else(|| ForgeError::NotFound(format!("label {name}")))
                })
            })
            .collect()
    }

    fn milestone_id(
        ctx: &ForgeContext,
        title: &str,
        cancel: &AtomicBool,
    ) -> Result<u64, ForgeError> {
        if let Ok(id) = title.trim().parse::<u64>() {
            return Ok(id);
        }
        Self::all_pages(
            ctx,
            Self::rest(ctx, "milestones")?.query("state", "all"),
            None,
            cancel,
        )?
        .iter()
        .find(|milestone| text(milestone, "title").eq_ignore_ascii_case(title.trim()))
        .and_then(|milestone| milestone["id"].as_u64())
        .ok_or_else(|| ForgeError::NotFound(format!("milestone {title}")))
    }

    fn inline(path: &str, line: u64, side: Side, body: &str) -> Value {
        let mut comment = json!({ "path": path, "body": body });
        comment[if side == Side::Left {
            "old_position"
        } else {
            "new_position"
        }] = line.into();
        comment
    }

    fn review_request(
        ctx: &ForgeContext,
        number: u64,
        event: &str,
        body: &str,
        head: Option<&str>,
        comments: Vec<Value>,
    ) -> Result<ApiRequest, ForgeError> {
        let mut payload = json!({ "event": event, "body": body, "comments": comments });
        if let Some(head) = head.filter(|head| !head.is_empty()) {
            payload["commit_id"] = head.into();
        }
        Ok(ApiRequest::new(
            Method::Post,
            repos_path(ctx, &format!("pulls/{number}/reviews"))?,
        )
        .json(payload)
        .body_file())
    }

    fn unsupported(capability: &'static str, reason: &str) -> ForgeError {
        ForgeError::Unsupported {
            capability,
            reason: reason.to_owned(),
        }
    }

    /// The requests a write sends, in order. Lookups a write needs first (label ids, the
    /// current assignees, a job's run) are made here; capability gates fail before any write.
    pub fn write_requests(
        ctx: &ForgeContext,
        write: &Write,
        cancel: &AtomicBool,
    ) -> Result<Vec<ApiRequest>, ForgeError> {
        let path = |tail: &str| repos_path(ctx, tail);
        let post = |tail: &str, body: Value| -> Result<ApiRequest, ForgeError> {
            Ok(ApiRequest::new(Method::Post, path(tail)?).json(body))
        };
        let patch = |tail: &str, body: Value| -> Result<ApiRequest, ForgeError> {
            Ok(ApiRequest::new(Method::Patch, path(tail)?).json(body))
        };
        Ok(match write {
            Write::CreateIssue {
                title,
                body,
                labels,
                assignees,
            } => {
                let mut payload = json!({ "title": title, "body": body });
                if !labels.is_empty() {
                    payload["labels"] = json!(Self::label_ids(ctx, labels, cancel)?);
                }
                if !assignees.is_empty() {
                    payload["assignees"] = json!(assignees);
                }
                vec![post("issues", payload)?]
            }
            Write::CreatePullRequest(request) => {
                let owner = request
                    .head_repository
                    .split_once('/')
                    .map(|(owner, _)| owner)
                    .ok_or_else(|| ForgeError::Failed("Invalid head repository".into()))?;
                let head = if request.head_repository == request.base_repository {
                    request.head_branch.clone()
                } else {
                    format!("{owner}:{}", request.head_branch)
                };
                // Gitea has no draft flag: a `WIP:` title is its draft.
                let title = if request.draft && !wip(&request.title) {
                    format!("WIP: {}", request.title)
                } else {
                    request.title.clone()
                };
                vec![
                    ApiRequest::new(
                        Method::Post,
                        format!("repos/{}/pulls", request.base_repository),
                    )
                    .json(json!({
                        "base": request.base_branch,
                        "head": head,
                        "title": title,
                        "body": request.body,
                    }))
                    .body_file(),
                ]
            }
            Write::Edit {
                number,
                title,
                body,
                ..
            } => vec![patch(
                &format!("issues/{number}"),
                json!({ "title": title, "body": body }),
            )?],
            Write::Comment { number, body, .. } => vec![post(
                &format!("issues/{number}/comments"),
                json!({ "body": body }),
            )?],
            Write::SetState {
                number,
                open,
                reason,
                ..
            } => {
                if !*open
                    && matches!(
                        reason,
                        Some(ClosedReason::NotPlanned | ClosedReason::Duplicate)
                    )
                {
                    return Err(Self::unsupported("issue_not_planned", NOT_PLANNED_REASON));
                }
                vec![patch(
                    &format!("issues/{number}"),
                    json!({ "state": if *open { "open" } else { "closed" } }),
                )?]
            }
            Write::SetLabels {
                number,
                add,
                remove,
                ..
            } => {
                if add.is_empty() && remove.is_empty() {
                    return Ok(Vec::new());
                }
                let current =
                    ctx.json(&ApiRequest::get(path(&format!("issues/{number}"))?), cancel)?;
                let mut names = labels(&current["labels"])
                    .into_iter()
                    .map(|label| label.name)
                    .filter(|name| !remove.iter().any(|gone| gone.eq_ignore_ascii_case(name)))
                    .collect::<Vec<_>>();
                for name in add {
                    if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                        names.push(name.clone());
                    }
                }
                let ids = Self::label_ids(ctx, &names, cancel)?;
                vec![
                    ApiRequest::new(Method::Put, path(&format!("issues/{number}/labels"))?)
                        .json(json!({ "labels": ids })),
                ]
            }
            Write::SetAssignees {
                number,
                add,
                remove,
                ..
            } => {
                let current =
                    ctx.json(&ApiRequest::get(path(&format!("issues/{number}"))?), cancel)?;
                let mut logins = users(&current["assignees"])
                    .into_iter()
                    .map(|user| user.login)
                    .filter(|login| !remove.contains(login))
                    .collect::<Vec<_>>();
                for login in add {
                    if !logins.contains(login) {
                        logins.push(login.clone());
                    }
                }
                vec![patch(
                    &format!("issues/{number}"),
                    json!({ "assignees": logins }),
                )?]
            }
            Write::SetMilestone {
                number, milestone, ..
            } => {
                let id = match milestone
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                {
                    Some(title) => Self::milestone_id(ctx, title, cancel)?,
                    // 0 clears it.
                    None => 0,
                };
                vec![patch(
                    &format!("issues/{number}"),
                    json!({ "milestone": id }),
                )?]
            }
            Write::SetReviewers {
                number,
                add,
                remove,
            } => {
                let split = |names: &[String]| {
                    let (teams, users): (Vec<&String>, Vec<&String>) =
                        names.iter().partition(|name| name.contains('/'));
                    json!({
                        "reviewers": users,
                        "team_reviewers": teams
                            .iter()
                            .map(|team| team.rsplit('/').next().unwrap_or(team))
                            .collect::<Vec<_>>(),
                    })
                };
                let mut requests = Vec::new();
                if !remove.is_empty() {
                    requests.push(
                        ApiRequest::new(
                            Method::Delete,
                            path(&format!("pulls/{number}/requested_reviewers"))?,
                        )
                        .json(split(remove)),
                    );
                }
                if !add.is_empty() {
                    requests.push(post(
                        &format!("pulls/{number}/requested_reviewers"),
                        split(add),
                    )?);
                }
                requests
            }
            Write::Subscribe { number, state, .. } => {
                let login = Self::viewer_login(ctx, cancel)?;
                let tail = format!("issues/{number}/subscriptions/{}", path_segment(&login));
                match state {
                    SubscriptionState::Subscribed => {
                        vec![ApiRequest::new(Method::Put, path(&tail)?)]
                    }
                    SubscriptionState::Unsubscribed => {
                        vec![ApiRequest::new(Method::Delete, path(&tail)?)]
                    }
                    SubscriptionState::Ignored => {
                        return Err(Self::unsupported(
                            "subscribe",
                            "Ignoring a thread is not offered here · unsubscribe instead",
                        ));
                    }
                }
            }
            Write::InlineComment {
                number,
                path: file,
                line,
                side,
                head,
                body,
            } => vec![Self::review_request(
                ctx,
                *number,
                "COMMENT",
                "",
                Some(head),
                vec![Self::inline(file, *line, *side, body)],
            )?],
            Write::Reply {
                number,
                thread,
                body,
            } => {
                // No reply endpoint: a review comment on the same line joins the thread.
                let (side, line, commit, file) = parse_thread(&thread.0).ok_or_else(|| {
                    ForgeError::Failed(format!("not a review thread here: {thread}"))
                })?;
                vec![Self::review_request(
                    ctx,
                    *number,
                    "COMMENT",
                    "",
                    commit.as_deref(),
                    vec![Self::inline(&file, line, side, body)],
                )?]
            }
            Write::Resolve { .. } => {
                return Err(Self::unsupported("resolve_thread", RESOLVE_REASON));
            }
            Write::CommitSuggestion { .. } => {
                return Err(Self::unsupported(
                    "commit_suggestion",
                    "This forge has no suggested changes to commit · o opens the pull request",
                ));
            }
            Write::SubmitReview {
                number,
                verdict,
                body,
                drafts,
                head,
            } => vec![Self::review_request(
                ctx,
                *number,
                match verdict {
                    Verdict::Approve => "APPROVED",
                    Verdict::RequestChanges => "REQUEST_CHANGES",
                    Verdict::Comment => "COMMENT",
                },
                body,
                head.as_deref(),
                drafts
                    .iter()
                    .map(|draft| {
                        Self::inline(&draft.path, draft.line, draft.side, &draft.range_body())
                    })
                    .collect(),
            )?],
            Write::Merge {
                number,
                method,
                head,
                delete_branch,
                title,
                message,
            } => {
                let mut payload = json!({
                    "Do": merge_word(*method),
                    "head_commit_id": head,
                    "delete_branch_after_merge": delete_branch,
                });
                if let Some(title) = title {
                    payload["MergeTitleField"] = title.clone().into();
                }
                if let Some(message) = message {
                    payload["MergeMessageField"] = message.clone().into();
                }
                vec![post(&format!("pulls/{number}/merge"), payload)?]
            }
            Write::Dispatch {
                workflow,
                reference,
                inputs,
            } => {
                server(ctx, cancel).require(&DISPATCH)?;
                let inputs = inputs
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect::<serde_json::Map<_, _>>();
                let workflow = crate::legacy::workflow_id(workflow);
                vec![post(
                    &format!("actions/workflows/{}/dispatches", path_segment(&workflow.0)),
                    json!({ "ref": reference, "inputs": inputs }),
                )?]
            }
            Write::Rerun { run, scope, .. } => {
                let server = server(ctx, cancel);
                match scope {
                    RerunScope::All => {
                        server.require(&RERUN)?;
                        vec![ApiRequest::new(
                            Method::Post,
                            path(&format!("actions/runs/{run}/rerun"))?,
                        )]
                    }
                    RerunScope::Failed => {
                        server.require(&RERUN_FAILED)?;
                        vec![ApiRequest::new(
                            Method::Post,
                            path(&format!("actions/runs/{run}/rerun-failed-jobs"))?,
                        )]
                    }
                }
            }
            Write::RerunJob(job) => {
                let server = server(ctx, cancel);
                server.require(&RERUN_JOB)?;
                if server.kind() == ProviderKind::Forgejo {
                    vec![ApiRequest::new(
                        Method::Post,
                        path(&format!("actions/jobs/{job}/rerun"))?,
                    )]
                } else {
                    // Gitea reruns a job under its run.
                    let value = ctx.json(
                        &ApiRequest::get(path(&format!("actions/jobs/{job}"))?),
                        cancel,
                    )?;
                    let run = text_or_number(&value, "run_id");
                    vec![ApiRequest::new(
                        Method::Post,
                        path(&format!("actions/runs/{run}/jobs/{job}/rerun"))?,
                    )]
                }
            }
            Write::CancelRun(run) => {
                server(ctx, cancel).require(&CANCEL)?;
                vec![ApiRequest::new(
                    Method::Post,
                    path(&format!("actions/runs/{run}/cancel"))?,
                )]
            }
            Write::ApproveRun { run, .. } => {
                server(ctx, cancel).require(&APPROVE)?;
                vec![ApiRequest::new(
                    Method::Post,
                    path(&format!("actions/runs/{run}/approve"))?,
                )]
            }
            Write::MarkRead(thread) => vec![
                ApiRequest::new(Method::Patch, format!("notifications/threads/{thread}"))
                    .query("to-status", "read"),
            ],
            Write::MarkAllRead => vec![ApiRequest::new(Method::Put, "notifications")],
            Write::Unsubscribe(_) => {
                return Err(Self::unsupported(
                    "unsubscribe",
                    "Notification threads are muted in the web UI here · o opens it",
                ));
            }
            Write::SetProjects { .. } => {
                return Err(Self::unsupported("projects", PROJECTS_REASON));
            }
            Write::SetViewed { .. } => Vec::new(),
            Write::Checkout { .. }
            | Write::Worktree { .. }
            | Write::Develop { .. }
            | Write::OpenInBrowser { .. } => {
                return Err(Self::unsupported(
                    "porcelain",
                    "runs through git and the browser, not the API",
                ));
            }
            Write::DownloadArtifact { artifact, .. } => {
                server(ctx, cancel).require(&ARTIFACTS)?;
                vec![super::download(ApiRequest::get(path(&format!(
                    "actions/artifacts/{artifact}/zip"
                ))?))]
            }
        })
    }
}

impl Provider for Gitea {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gitea
    }

    fn terms(&self) -> &'static Terms {
        &GITEA_TERMS
    }

    fn capabilities(&self, ctx: &ForgeContext) -> Capabilities {
        let capabilities = capabilities(server(ctx, &AtomicBool::new(false)));
        if ctx.anonymous() {
            capabilities.read_only(ctx.endpoints.kind)
        } else {
            capabilities
        }
    }

    fn plan(&self, ctx: &ForgeContext, read: &Read, _cond: &Conditional) -> Plan {
        match Self::requests(ctx, read) {
            Ok(requests) if requests.is_empty() => Plan {
                requests,
                note: Some(match read {
                    Read::Viewed(_) => "viewed files are kept on this computer",
                    _ => "read locally from the base branch",
                }),
            },
            Ok(requests) => Plan {
                requests,
                note: None,
            },
            Err(_) => Plan::default(),
        }
    }

    fn incremental(&self, _ctx: &ForgeContext, read: &Read) -> bool {
        matches!(read, Read::Notifications { filter, page } if filter.since.is_none() && page.number <= 1)
    }

    fn read(
        &self,
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        ctx.start_deadline(super::read_budget(read));
        let result = if ctx.anonymous() {
            Self::anonymous(ctx, read, cond, cancel)
        } else {
            Self::fetch(ctx, read, cond, cancel)
        };
        ctx.clear_deadline();
        result
    }

    fn write(
        &self,
        ctx: &ForgeContext,
        write: &Write,
        cancel: &AtomicBool,
    ) -> Result<Receipt, ForgeError> {
        ctx.require_credential(
            write,
            crate::worker::local_write(ProviderKind::Gitea, write),
        )?;
        let invalidates = write.invalidates();
        if let Write::SetViewed {
            number,
            path,
            viewed,
            ..
        } = write
        {
            save_viewed(ctx, *number, path, *viewed)?;
            return Ok(Receipt {
                summary: "Saved on this computer".into(),
                web_url: None,
                created_number: None,
                invalidates,
            });
        }
        if let Write::DownloadArtifact {
            name, directory, ..
        } = write
        {
            let mut requests = Self::write_requests(ctx, write, cancel)?;
            let response = ctx.ok(&requests.remove(0), cancel)?;
            return super::save_artifact(&response.body, name, directory);
        }
        if let Write::ApproveRun { run, expected } = write {
            // As on GitHub: approve only the head the user looked at.
            server(ctx, cancel).require(&APPROVE)?;
            let value = ctx.json(&Self::rest(ctx, &format!("actions/runs/{run}"))?, cancel)?;
            let sha = text(&value, "head_sha");
            let branch = text(&value, "head_branch");
            let owner = text(&value["head_repository"]["owner"], "login");
            let mut changed = Vec::new();
            if !sha.is_empty() && !sha.eq_ignore_ascii_case(&expected.sha) {
                changed.push("head commit");
            }
            if !branch.is_empty() && branch != expected.branch {
                changed.push("branch");
            }
            if !owner.is_empty()
                && !expected.head_owner.is_empty()
                && !owner.eq_ignore_ascii_case(&expected.head_owner)
            {
                changed.push("fork owner");
            }
            if !changed.is_empty() {
                return Err(ForgeError::Conflict(format!(
                    "the run's {} changed since you looked · reload before approving",
                    changed.join(" and ")
                )));
            }
        }
        let mut last = Value::Null;
        for request in Self::write_requests(ctx, write, cancel)? {
            Self::cancelled(cancel)?;
            last = ctx.json(&request, cancel)?;
        }
        Ok(Receipt {
            summary: "Saved".into(),
            web_url: optional(&last, "html_url"),
            created_number: match write {
                Write::CreateIssue { .. } | Write::CreatePullRequest(_) => last["number"].as_u64(),
                _ => None,
            },
            invalidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Body;
    use crate::transport::HostKey;

    fn ctx(kind: ProviderKind) -> ForgeContext {
        let endpoints = crate::detect::endpoints_for(
            kind,
            &HostKey::new("codeberg.org"),
            Some("https://codeberg.org/api/v1"),
            &crate::credential::Env::from_pairs::<&str, &str>([]),
        );
        ForgeContext::offline(endpoints, super::super::RepoRef::parse("orbit/orbit"))
    }

    fn body(request: &ApiRequest) -> Value {
        match &request.body {
            Some(Body::Json(value)) => value.clone(),
            _ => Value::Null,
        }
    }

    #[test]
    fn versions_parse_by_family() {
        assert_eq!(
            Server::parse("1.22.6"),
            Some(Server::Gitea {
                major: 1,
                minor: 22
            })
        );
        assert_eq!(
            Server::parse("29.0.0+dev-160-g55186ca39b"),
            Some(Server::Gitea {
                major: 29,
                minor: 0
            })
        );
        assert_eq!(
            Server::parse("17.0.0-26-363f3dc30b+gitea-1.22.0"),
            Some(Server::Forgejo { major: 17 })
        );
        assert_eq!(
            Server::parse("7.0.5+gitea-1.21.11"),
            Some(Server::Forgejo { major: 7 })
        );
        assert_eq!(Server::parse("garbage"), None);
    }

    #[test]
    fn gates_follow_versions_md() {
        let forgejo = |major| capabilities(Server::Forgejo { major });
        let gitea = |minor| capabilities(Server::Gitea { major: 1, minor });
        let main = capabilities(Server::Gitea {
            major: 29,
            minor: 0,
        });
        assert_eq!(forgejo(17).rerun_all, Cap::Yes);
        assert!(!forgejo(16).rerun_all.allowed());
        assert!(!forgejo(17).rerun_failed.allowed());
        assert_eq!(forgejo(16).cancel_run, Cap::Yes);
        assert!(!forgejo(17).approve_fork_run.allowed());
        assert!(!forgejo(7).actions.allowed());
        assert_eq!(forgejo(8).actions, Cap::Yes);
        assert_eq!(
            forgejo(8).runs_list,
            Cap::Yes,
            "the tasks fallback lists runs"
        );
        assert_eq!(gitea(26).rerun_failed, Cap::Yes);
        assert!(!gitea(27).cancel_run.allowed());
        assert_eq!(main.cancel_run, Cap::Yes);
        assert_eq!(main.approve_fork_run, Cap::Yes);
        assert!(!gitea(22).actions.allowed());
        assert_eq!(gitea(24).job_log, Cap::Yes);
        assert_eq!(gitea(24).job_steps, StepsKind::Unavailable);
        assert_eq!(gitea(25).job_steps, StepsKind::Known);
        assert!(!main.resolve_thread.allowed());
        assert_eq!(main.resolve_thread.reason(), Some(RESOLVE_REASON));
        let unknown = capabilities(Server::Unknown(ProviderKind::Forgejo));
        assert_eq!(unknown.rerun_all, Cap::Probe);
        assert!(!unknown.rerun_failed.allowed(), "never on Forgejo");
        let error = Server::Gitea {
            major: 1,
            minor: 24,
        }
        .require(&RERUN)
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unsupported here · Rerunning needs Gitea 1.26 or newer · o opens the run in the \
             browser · this server runs Gitea 1.24"
        );
    }

    #[test]
    fn search_syntax_becomes_gitea_parameters() {
        let filter = |query: &str| Filter {
            state: StateFilter::Open,
            query: query.into(),
        };
        let query = translate(
            &filter("label:bug label:ui author:maya fix crash sort:updated-desc"),
            false,
        );
        assert_eq!(
            query.params,
            [
                ("created_by".to_owned(), "maya".to_owned()),
                ("labels".to_owned(), "bug,ui".to_owned())
            ]
        );
        assert_eq!(query.keywords, ["fix", "crash"]);
        let inbox = translate(
            &filter("review-requested:@me repo:orbit/orbit is:closed"),
            true,
        );
        assert_eq!(
            inbox.params,
            [
                ("review_requested".to_owned(), "true".to_owned()),
                ("owner".to_owned(), "orbit".to_owned())
            ]
        );
        assert_eq!(inbox.repo.as_deref(), Some("orbit/orbit"));
        assert_eq!(inbox.state, Some(StateFilter::Closed));
        assert!(translate(&filter("author:@me"), false).needs_viewer);
        let ctx = ctx(ProviderKind::Forgejo);
        let (request, _) =
            Gitea::list_request(&ctx, ItemKind::PullRequest, &filter(""), None).unwrap();
        assert_eq!(
            request.path_and_query(),
            "repos/orbit/orbit/pulls?state=open"
        );
        let (request, _) =
            Gitea::list_request(&ctx, ItemKind::PullRequest, &filter("label:bug"), None).unwrap();
        assert_eq!(
            request.path_and_query(),
            "repos/orbit/orbit/issues?type=pulls&state=open&labels=bug"
        );
    }

    #[test]
    fn review_comments_group_into_line_threads() {
        let comment = |id: u64, path: &str, position: u64, original: u64, resolver: Value| {
            json!({"id": id, "path": path, "position": position, "original_position": original,
                   "body": format!("c{id}"), "user": {"login": "sam"}, "resolver": resolver,
                   "created_at": format!("2026-11-0{id}T00:00:00Z"), "commit_id": "abc"})
        };
        let threads = threads(&[
            comment(2, "a.rs", 12, 0, Value::Null),
            comment(1, "a.rs", 12, 0, Value::Null),
            comment(3, "a.rs", 0, 7, json!({"login": "maya"})),
        ]);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].id.0, "R12@abc:a.rs");
        assert_eq!(threads[0].comments.len(), 2);
        assert_eq!(threads[0].comments[0].body, "c1");
        assert_eq!(threads[0].line, Some(12));
        assert!(!threads[0].resolvable);
        assert_eq!(
            (threads[1].side, threads[1].original_line),
            (Side::Left, Some(7))
        );
        assert_eq!(threads[1].resolved, Some(true));
        assert_eq!(
            parse_thread("R12@abc:src/a:b.rs"),
            Some((Side::Right, 12, Some("abc".into()), "src/a:b.rs".into()))
        );
    }

    #[test]
    fn writes_map_onto_gitea_bodies_and_gates() {
        let ctx = ctx(ProviderKind::Gitea);
        servers().lock().unwrap().insert(
            ctx.endpoints.api_base.clone(),
            Server::Gitea {
                major: 1,
                minor: 24,
            },
        );
        let cancel = AtomicBool::new(false);
        let requests = |write: Write| Gitea::write_requests(&ctx, &write, &cancel);
        let merge = requests(Write::Merge {
            number: 13,
            method: MergeMethod::Squash,
            head: "c92ad11".into(),
            delete_branch: true,
            title: None,
            message: None,
        })
        .unwrap();
        assert_eq!(
            merge[0].path_and_query(),
            "repos/orbit/orbit/pulls/13/merge"
        );
        assert_eq!(
            body(&merge[0]),
            json!({"Do": "squash", "head_commit_id": "c92ad11", "delete_branch_after_merge": true})
        );
        let review = requests(Write::SubmitReview {
            number: 13,
            verdict: Verdict::Approve,
            body: "Ship it".into(),
            drafts: vec![InlineDraft {
                path: "src/lib.rs".into(),
                line: 3,
                side: Side::Left,
                body: "old".into(),
                start_line: None,
            }],
            head: Some("c92ad11".into()),
        })
        .unwrap();
        assert_eq!(
            body(&review[0]),
            json!({"event": "APPROVED", "body": "Ship it", "commit_id": "c92ad11",
                   "comments": [{"path": "src/lib.rs", "old_position": 3, "body": "old"}]})
        );
        let reply = requests(Write::Reply {
            number: 13,
            thread: ItemId::new("R12@c92ad11:src/lib.rs"),
            body: "Done.".into(),
        })
        .unwrap();
        assert_eq!(
            body(&reply[0])["comments"],
            json!([{"path": "src/lib.rs", "new_position": 12, "body": "Done."}])
        );
        let read = requests(Write::MarkRead(ItemId::new("9001"))).unwrap();
        assert_eq!(
            read[0].path_and_query(),
            "notifications/threads/9001?to-status=read"
        );
        // Gated before anything is sent (the offline transport would fail otherwise).
        assert!(matches!(
            requests(Write::Resolve {
                thread: ItemId::new("R1:a"),
                resolved: true
            }),
            Err(ForgeError::Unsupported {
                capability: "resolve_thread",
                ..
            })
        ));
        assert!(matches!(
            requests(Write::Rerun {
                run: ItemId::new("91"),
                scope: RerunScope::All,
                debug: false
            }),
            Err(ForgeError::Unsupported {
                capability: "rerun_all",
                ..
            })
        ));
        assert!(matches!(
            requests(Write::SetState {
                kind: ItemKind::Issue,
                number: 1,
                open: false,
                reason: Some(ClosedReason::NotPlanned)
            }),
            Err(ForgeError::Unsupported { .. })
        ));
        let draft = requests(Write::CreatePullRequest(
            crate::pull_request::CreatePullRequest {
                host: "codeberg.org".into(),
                base_repository: "orbit/orbit".into(),
                base_branch: "main".into(),
                head_repository: "maya/orbit".into(),
                head_branch: "fix".into(),
                title: "Fix it".into(),
                body: String::new(),
                draft: true,
                push: None,
            },
        ))
        .unwrap();
        assert_eq!(body(&draft[0])["title"], "WIP: Fix it");
        assert_eq!(body(&draft[0])["head"], "maya:fix");
    }

    #[test]
    fn forgejo_runs_and_tasks_map_to_the_model() {
        let forgejo = json!({"id": 200481, "title": "test", "workflow_id": "test.yml",
            "index_in_repo": 24108, "prettyref": "#1790", "commit_sha": "80c9", "status": "failure",
            "event": "pull_request", "trigger_event": "pull_request", "duration": 3_000_000_000u64,
            "need_approval": true, "html_url": "https://x/runs/24108", "created": "2026-11-14T04:21:52Z"});
        let run = run(&forgejo);
        assert_eq!(run.number, 24108);
        assert_eq!(run.pr_number, Some(1790));
        assert_eq!(run.conclusion, Some(CiState::Failed));
        assert_eq!(run.conclusion_raw.as_deref(), Some("failure"));
        assert_eq!(run.workflow.as_ref().unwrap().id.0, "test.yml");
        assert!(run.needs_approval);
        let tasks = [
            json!({"id": 3, "name": "arm64", "run_number": 7, "status": "failure", "workflow_id": "ci.yml",
                   "display_title": "Seven", "run_started_at": "2026-11-14T05:00:00Z"}),
            json!({"id": 2, "name": "lint", "run_number": 7, "status": "success",
                   "run_started_at": "2026-11-14T04:00:00Z"}),
            json!({"id": 1, "name": "lint", "run_number": 6, "status": "running"}),
        ];
        let runs = task_runs(&tasks);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0.id.0, "7");
        assert_eq!(runs[0].0.conclusion, Some(CiState::Failed));
        assert_eq!(runs[0].0.created.as_str(), "2026-11-14T04:00:00Z");
        assert_eq!(runs[0].1.len(), 2);
        assert_eq!(runs[1].0.status, RunStatus::InProgress);
        assert_eq!(runs[0].1[0].id.0, "task-3");
        let job = job(&json!({"id": 424038, "name": "arm64", "status": "failure",
            "steps": [{"number": 0, "name": "Set up job", "status": "failure"}]}));
        assert_eq!(job.conclusion, Some(CiState::Failed));
        assert!(matches!(&job.steps, Steps::Known(steps) if steps.len() == 1));
    }

    #[test]
    fn rows_read_pulls_issues_and_drafts() {
        let pull = row(
            &json!({"number": 1402, "title": "WIP: stable dirs", "state": "open",
            "user": {"login": "maya"}, "head": {"ref": "enhance/x"}, "comments": 3,
            "labels": [{"id": 1, "name": "bug", "color": "ee0701"}], "updated_at": "2026-11-10T12:49:29Z"}),
        );
        assert_eq!(pull.kind, Some(ItemKind::PullRequest));
        assert!(pull.draft);
        assert_eq!(pull.lifecycle, Lifecycle::Draft);
        assert_eq!(pull.labels[0].id, Some(ItemId::new(1)));
        let merged =
            row(&json!({"number": 2, "state": "closed", "pull_request": {"merged": true}}));
        assert_eq!(merged.lifecycle, Lifecycle::Merged);
        let issue = row(&json!({"number": 3, "state": "open", "pull_request": null}));
        assert_eq!(issue.kind, Some(ItemKind::Issue));
    }
}
