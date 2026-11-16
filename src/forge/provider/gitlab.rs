//! GitLab (gitlab.com and self-managed, nested groups included) over REST v4 and GraphQL
//! (FORGE.md §4.2): one slim GraphQL document per list, REST for details, discussions, diffs,
//! pipelines and every write except "request changes", which GitLab only offers in GraphQL.
//! The project id is the URL-encoded full path (`orbit%2Fteam%2Forbit`), so a group of any depth
//! works without a lookup. REST pages follow `x-next-page` (empty on the last page) and report
//! `x-total` when GitLab counts (it stops counting past 10 000 rows).

mod anonymous;

use super::Conditional;
use super::Fetched;
use super::parallel;
use super::reactions;
use super::ForgeContext;
use super::Plan;
use super::Provider;
use super::ProviderKind;
use super::common::edited;
use super::common::id_of;
use super::common::is_bot;
use super::common::link_target;
use super::common::optional;
use super::common::optional_time;
use super::common::path_segment;
use super::common::text;
use super::common::timestamp;
use crate::forge::capability::Cap;
use crate::forge::capability::Capabilities;
use crate::forge::capability::GITLAB_TERMS;
use crate::forge::capability::RunFilters;
use crate::forge::capability::StepsKind;
use crate::forge::capability::Terms;
use crate::forge::model::*;
use crate::forge::request::*;
use crate::forge::transport::Accept;
use crate::forge::transport::ApiRequest;
use crate::forge::transport::ApiResponse;
use crate::forge::transport::Method;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct GitLab;

/// GitLab's largest page for REST and GraphQL connections.
const PAGE: u32 = 100;
/// Bounded fan-out for paginated reads (discussions, notes, diffs, jobs).
const MAX_PAGES: usize = 30;
/// How long a "rebase then merge" waits for GitLab's asynchronous rebase.
const REBASE_WAIT: Duration = Duration::from_secs(30);

/// `project.mergeRequests` row fields: review, pipeline and ± slots in one round trip.
const MR_ROW_FIELDS: &str = "iid id title description state draft webUrl createdAt updatedAt userNotesCount \
mergeStatusEnum sourceBranch author { username name } labels(first: 20) { nodes { title color } } approved \
approvedBy { nodes { username } } reviewers { nodes { username } } headPipeline { status } \
diffStatsSummary { additions deletions fileCount }";

const ISSUE_ROW_FIELDS: &str = "iid id title description state webUrl createdAt updatedAt userNotesCount \
author { username name } labels(first: 20) { nodes { title color } }";

const REQUEST_CHANGES_DOCUMENT: &str = "mutation MergeRequestRequestChanges($projectPath: ID!, $iid: String!) { \
mergeRequestRequestChanges(input: { projectPath: $projectPath, iid: $iid }) { errors mergeRequest { iid } } }";

/// `Cap::Probe` for request changes (VERSIONS.md: introspect, never trust the version).
const MUTATIONS_DOCUMENT: &str =
    "query MutationFields { __type(name: \"Mutation\") { fields { name } } }";

const REQUEST_CHANGES: &str = "review_request_changes";
const DRAFT_NOTES: &str = "batched_review";

// ------------------------------------------------------------------------------ JSON helpers

/// GraphQL spells iids as strings, REST as numbers.
fn count(value: &Value, key: &str) -> u64 {
    match &value[key] {
        Value::Number(number) => number.as_u64().unwrap_or_default(),
        Value::String(text) => text.parse().unwrap_or_default(),
        _ => 0,
    }
}

fn nodes(value: &Value) -> &[Value] {
    value["nodes"]
        .as_array()
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn user(value: &Value) -> User {
    User {
        login: optional(value, "username")
            .or_else(|| optional(value, "login"))
            .unwrap_or_default(),
        id: value["id"].as_u64().map(ItemId::new),
        name: optional(value, "name"),
        bot: is_bot(value),
    }
}

/// REST labels are names (or `{name, color}` with `with_labels_details`); GraphQL nodes have
/// `title`.
fn labels(value: &Value) -> Vec<Label> {
    nodes(value)
        .iter()
        .map(|label| match label {
            Value::String(name) => Label {
                id: None,
                name: name.clone(),
                color: None,
            },
            label => Label {
                id: label["id"].as_u64().map(ItemId::new),
                name: optional(label, "name").unwrap_or_else(|| text(label, "title")),
                color: optional(label, "color"),
            },
        })
        .collect()
}

fn milestone(value: &Value) -> Option<Milestone> {
    value.is_object().then(|| Milestone {
        id: value["id"].as_u64().map(ItemId::new),
        title: text(value, "title"),
        due: optional_time(value, "due_date").or_else(|| optional_time(value, "dueDate")),
        open: value["state"]
            .as_str()
            .is_none_or(|state| state.eq_ignore_ascii_case("active")),
    })
}

/// `opened`, `closed`, `locked`, `merged` (GraphQL enums are the same words).
fn lifecycle(value: &Value) -> Lifecycle {
    let draft =
        value["draft"].as_bool() == Some(true) || value["work_in_progress"].as_bool() == Some(true);
    match text(value, "state").to_ascii_lowercase().as_str() {
        "merged" => Lifecycle::Merged,
        "closed" | "locked" => Lifecycle::Closed,
        _ if draft => Lifecycle::Draft,
        _ => Lifecycle::Open,
    }
}

/// Pipeline and job statuses (REST lower case, GraphQL upper case).
pub fn pipeline_state(word: &str) -> CiState {
    match word.to_ascii_lowercase().as_str() {
        "success" => CiState::Passed,
        "failed" => CiState::Failed,
        "canceled" | "canceling" => CiState::Cancelled,
        "skipped" => CiState::Skipped,
        "manual" => CiState::ActionRequired,
        "created"
        | "waiting_for_resource"
        | "preparing"
        | "pending"
        | "running"
        | "scheduled"
        | "waiting_for_callback" => CiState::Pending,
        _ => CiState::Unknown,
    }
}

/// A job's state as a check: an allowed failure is GitLab's "passed with warnings" and a manual
/// job is optional, so neither may fail or block the rollup.
fn job_state(value: &Value) -> CiState {
    let allowed = value["allow_failure"].as_bool() == Some(true);
    match pipeline_state(value["status"].as_str().unwrap_or_default()) {
        CiState::Failed if allowed => CiState::Skipped,
        CiState::ActionRequired => CiState::Skipped,
        state => state,
    }
}

fn run_status(word: &str) -> RunStatus {
    match word.to_ascii_lowercase().as_str() {
        "created" | "waiting_for_resource" | "preparing" => RunStatus::Queued,
        "pending" => RunStatus::Pending,
        "running" | "canceling" => RunStatus::InProgress,
        "scheduled" | "manual" | "waiting_for_callback" => RunStatus::Waiting,
        _ => RunStatus::Completed,
    }
}

/// The conclusion words the run views already know (GitHub's), so GitLab pipelines render with
/// the same chips.
fn conclusion_word(word: &str) -> Option<&'static str> {
    match word.to_ascii_lowercase().as_str() {
        "success" => Some("success"),
        "failed" => Some("failure"),
        "canceled" => Some("cancelled"),
        "skipped" => Some("skipped"),
        "manual" => Some("action_required"),
        _ => None,
    }
}

fn mergeable(value: &Value) -> Mergeable {
    if value["has_conflicts"].as_bool() == Some(true) {
        return Mergeable::Conflicting;
    }
    match text(value, "detailed_merge_status").as_str() {
        "mergeable" => Mergeable::Clean,
        "conflict" | "broken_status" => Mergeable::Conflicting,
        "need_rebase" => Mergeable::Behind,
        "draft_status" => Mergeable::Draft,
        "ci_must_pass" | "ci_still_running" => Mergeable::Unstable,
        "checking" | "unchecked" | "preparing" | "approvals_syncing" | "" => Mergeable::Unknown,
        _ => Mergeable::Blocked,
    }
}

/// `orbit/team/orbit!564` / `orbit/team/orbit#603` → the project path.
fn reference_repo(value: &Value) -> Option<String> {
    let full = value["references"]["full"].as_str()?;
    full.rsplit_once(['!', '#'])
        .map(|(repo, _)| repo.to_owned())
        .filter(|repo| !repo.is_empty())
}

fn item_path(kind: ItemKind, number: u64) -> String {
    match kind {
        ItemKind::Issue => format!("issues/{number}"),
        ItemKind::PullRequest => format!("merge_requests/{number}"),
    }
}

// ------------------------------------------------------------------------------ rows and details

/// A GraphQL `mergeRequests`/`issues` node.
fn graphql_row(node: &Value, kind: ItemKind, repo: &str) -> ItemRow {
    let pipeline = node["headPipeline"]["status"].as_str().map(pipeline_state);
    let stats = &node["diffStatsSummary"];
    let review = if node["approved"].as_bool() == Some(true) {
        Some(ReviewState::Approved)
    } else if !nodes(&node["reviewers"]).is_empty() {
        Some(ReviewState::Requested)
    } else {
        None
    };
    ItemRow {
        kind: Some(kind),
        number: count(node, "iid"),
        id: id_of(node, "id"),
        title: text(node, "title"),
        author: user(&node["author"]),
        lifecycle: lifecycle(node),
        labels: labels(&node["labels"]),
        updated: timestamp(node, "updatedAt"),
        comments: count(node, "userNotesCount"),
        repo: Some(repo.to_owned()),
        web_url: text(node, "webUrl"),
        review: (kind == ItemKind::PullRequest).then_some(review).flatten(),
        ci: pipeline,
        ci_counts: pipeline.map(|state| vec![(state, 1)]).unwrap_or_default(),
        additions: stats["additions"].as_u64(),
        deletions: stats["deletions"].as_u64(),
        changed_files: stats["fileCount"].as_u64(),
        draft: node["draft"].as_bool().unwrap_or(false),
        body_preview: node["description"].as_str().map(str::to_owned),
        head_branch: optional(node, "sourceBranch"),
        review_tally: None,
        created: Some(timestamp(node, "createdAt")).filter(|created| !created.is_empty()),
        linked: None,
    }
}

/// A REST merge request or issue (lists, search, details).
fn rest_row(value: &Value, kind: ItemKind) -> ItemRow {
    let pipeline = value["head_pipeline"]["status"]
        .as_str()
        .map(pipeline_state);
    ItemRow {
        kind: Some(kind),
        number: count(value, "iid"),
        id: id_of(value, "id"),
        title: text(value, "title"),
        author: user(&value["author"]),
        lifecycle: lifecycle(value),
        labels: labels(&value["labels"]),
        updated: timestamp(value, "updated_at"),
        comments: count(value, "user_notes_count"),
        repo: reference_repo(value),
        web_url: text(value, "web_url"),
        review: None,
        ci: pipeline,
        ci_counts: pipeline.map(|state| vec![(state, 1)]).unwrap_or_default(),
        additions: None,
        deletions: None,
        changed_files: value["changes_count"]
            .as_str()
            .and_then(|count| count.trim_end_matches('+').parse().ok()),
        draft: value["draft"].as_bool().unwrap_or(false)
            || value["work_in_progress"].as_bool().unwrap_or(false),
        body_preview: value["description"].as_str().map(str::to_owned),
        head_branch: optional(value, "source_branch"),
        review_tally: None,
        created: Some(timestamp(value, "created_at")).filter(|created| !created.is_empty()),
        // An issue counts the merge requests that mention it as closing it.
        linked: value["merge_requests_count"].as_u64().filter(|_| kind == ItemKind::Issue),
    }
}

fn link(value: &Value, kind: ItemKind) -> LinkRef {
    LinkRef {
        number: count(value, "iid"),
        title: text(value, "title"),
        repo: reference_repo(value),
        web_url: optional(value, "web_url"),
        kind: Some(kind),
    }
}

fn issue(value: &Value, closed_by: &[Value]) -> Issue {
    Issue {
        row: rest_row(value, ItemKind::Issue),
        body: text(value, "description"),
        assignees: value["assignees"]
            .as_array()
            .into_iter()
            .flatten()
            .map(user)
            .collect(),
        milestone: milestone(&value["milestone"]),
        linked_prs: closed_by
            .iter()
            .map(|pull| link(pull, ItemKind::PullRequest))
            .collect(),
        created: timestamp(value, "created_at"),
        closed_reason: None,
        projects: Vec::new(),
        subscribed: value["subscribed"].as_bool(),
        reactions: Vec::new(),
    }
}

/// `+`/`-` lines of one file's unified diff body.
fn diff_stats(diff: &str) -> (u64, u64) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    (added, removed)
}

fn changed_file(value: &Value) -> ChangedFile {
    let (additions, deletions) = diff_stats(value["diff"].as_str().unwrap_or_default());
    let status = if value["new_file"].as_bool() == Some(true) {
        FileStatus::Added
    } else if value["deleted_file"].as_bool() == Some(true) {
        FileStatus::Deleted
    } else if value["renamed_file"].as_bool() == Some(true) {
        FileStatus::Renamed
    } else {
        FileStatus::Modified
    };
    ChangedFile {
        path: text(value, "new_path"),
        old_path: (status == FileStatus::Renamed).then(|| text(value, "old_path")),
        status,
        additions,
        deletions,
        viewed: None,
    }
}

/// GitLab's `/diffs` entries carry hunks only; rebuild the git headers `hunks.rs` parses.
pub fn assemble_diff(files: &[Value]) -> String {
    let mut out = String::new();
    for file in files {
        let old = text(file, "old_path");
        let new = text(file, "new_path");
        let added = file["new_file"].as_bool() == Some(true);
        let deleted = file["deleted_file"].as_bool() == Some(true);
        let renamed = file["renamed_file"].as_bool() == Some(true);
        out.push_str(&format!("diff --git a/{old} b/{new}\n"));
        let (a_mode, b_mode) = (text(file, "a_mode"), text(file, "b_mode"));
        if added {
            out.push_str(&format!("new file mode {b_mode}\n"));
        } else if deleted {
            out.push_str(&format!("deleted file mode {a_mode}\n"));
        } else if a_mode != b_mode && !a_mode.is_empty() && !b_mode.is_empty() {
            out.push_str(&format!("old mode {a_mode}\nnew mode {b_mode}\n"));
        }
        if renamed {
            out.push_str(&format!("rename from {old}\nrename to {new}\n"));
        }
        let diff = file["diff"].as_str().unwrap_or_default();
        if diff.is_empty() {
            if file["too_large"].as_bool() == Some(true)
                || file["collapsed"].as_bool() == Some(true)
            {
                out.push_str(&format!("Binary files a/{old} and b/{new} differ\n"));
            }
            continue;
        }
        let minus = if added {
            "/dev/null".to_owned()
        } else {
            format!("a/{old}")
        };
        let plus = if deleted {
            "/dev/null".to_owned()
        } else {
            format!("b/{new}")
        };
        out.push_str(&format!("--- {minus}\n+++ {plus}\n"));
        out.push_str(diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn reviewer_state(word: &str) -> ReviewState {
    match word {
        "approved" => ReviewState::Approved,
        "requested_changes" => ReviewState::ChangesRequested,
        "reviewed" => ReviewState::Commented,
        "unapproved" => ReviewState::Dismissed,
        _ => ReviewState::Requested,
    }
}

fn check(job: &Value, pipeline: &ItemId) -> Check {
    let state = job_state(job);
    Check {
        name: text(job, "name"),
        workflow: optional(job, "stage"),
        state,
        raw_state: Some(crate::forge::legacy::ci_word(state).into()),
        started: optional_time(job, "started_at"),
        duration: job["duration"].as_f64().map(Duration::from_secs_f64),
        completed: optional_time(job, "finished_at"),
        details_url: optional(job, "web_url"),
        run_link: Some(RunLink {
            run: pipeline.clone(),
            job: Some(id_of(job, "id")),
        }),
        required: job["allow_failure"].as_bool() != Some(true),
    }
}

/// The parts of a merge request detail beyond `GET merge_requests/{iid}`.
#[derive(Default)]
struct DetailParts {
    approvals: Value,
    reviewers: Vec<Value>,
    commits: Vec<Value>,
    diffs: Vec<Value>,
    jobs: Vec<Value>,
    closes: Vec<Value>,
}

fn pull_request(value: &Value, parts: DetailParts) -> PullRequest {
    let mut row = rest_row(value, ItemKind::PullRequest);
    let pipeline = &value["head_pipeline"];
    let pipeline_id = id_of(pipeline, "id");
    let checks = checks::dedupe(
        parts
            .jobs
            .iter()
            .map(|job| check(job, &pipeline_id))
            .collect(),
    );
    row.ci = pipeline["status"]
        .as_str()
        .map(pipeline_state)
        .or_else(|| checks::rollup(&checks));
    if !checks.is_empty() {
        let mut counts: Vec<(CiState, u64)> = Vec::new();
        for check in &checks {
            match counts.iter_mut().find(|(state, _)| *state == check.state) {
                Some((_, total)) => *total += 1,
                None => counts.push((check.state, 1)),
            }
        }
        row.ci_counts = counts;
    }
    let reviewers: Vec<ReviewerSlot> = if parts.reviewers.is_empty() {
        value["reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|reviewer| ReviewerSlot {
                reviewer: user(reviewer).login,
                state: ReviewState::Requested,
            })
            .collect()
    } else {
        parts
            .reviewers
            .iter()
            .map(|slot| ReviewerSlot {
                reviewer: user(&slot["user"]).login,
                state: reviewer_state(slot["state"].as_str().unwrap_or_default()),
            })
            .collect()
    };
    let mut reviews: Vec<Review> = parts.approvals["approved_by"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|approval| {
            let author = user(&approval["user"]);
            Review {
                id: ItemId::new(format!("approval-{}", author.login)),
                author,
                state: ReviewState::Approved,
                body: String::new(),
                submitted: timestamp(approval, "approved_at"),
            }
        })
        .collect();
    for slot in &parts.reviewers {
        if slot["state"] == "requested_changes" {
            let author = user(&slot["user"]);
            reviews.push(Review {
                id: ItemId::new(format!("changes-{}", author.login)),
                author,
                state: ReviewState::ChangesRequested,
                body: String::new(),
                submitted: timestamp(slot, "created_at"),
            });
        }
    }
    row.review = if reviews
        .iter()
        .any(|review| review.state == ReviewState::ChangesRequested)
    {
        Some(ReviewState::ChangesRequested)
    } else if parts.approvals["approved"].as_bool() == Some(true)
        || (parts.approvals["approvals_left"].as_u64() == Some(0) && !reviews.is_empty())
    {
        Some(ReviewState::Approved)
    } else if parts.approvals["approvals_left"]
        .as_u64()
        .is_some_and(|left| left > 0)
        || !reviewers.is_empty()
    {
        Some(ReviewState::Requested)
    } else {
        None
    };
    let files = parts.diffs.iter().map(changed_file).collect::<Vec<_>>();
    if row.changed_files.is_none() && !files.is_empty() {
        row.changed_files = Some(files.len() as u64);
    }
    let (additions, deletions) = files.iter().fold((0, 0), |(a, d), file| {
        (a + file.additions, d + file.deletions)
    });
    if !files.is_empty() {
        row.additions = Some(additions);
        row.deletions = Some(deletions);
    }
    let head = text(value, "sha");
    let refs = &value["diff_refs"];
    PullRequest {
        body: text(value, "description"),
        base: RefInfo {
            repo: None,
            branch: text(value, "target_branch"),
            sha: text(refs, "base_sha"),
        },
        head: RefInfo {
            // A fork's source project is only known by id here.
            repo: None,
            branch: text(value, "source_branch"),
            sha: head.clone(),
        },
        mergeable: mergeable(value),
        reviewers,
        reviews,
        checks,
        files,
        commits: parts
            .commits
            .iter()
            .map(|commit| {
                let message = text(commit, "message");
                let body = message
                    .split_once('\n')
                    .map(|(_, body)| body.trim().to_owned())
                    .unwrap_or_default();
                CommitSummary {
                    sha: text(commit, "id"),
                    headline: optional(commit, "title")
                        .unwrap_or_else(|| message.lines().next().unwrap_or_default().into()),
                    body,
                    committed: optional_time(commit, "committed_date")
                        .unwrap_or_else(|| timestamp(commit, "created_at")),
                    author: optional(commit, "author_name").map(User::login),
                }
            })
            .collect(),
        assignees: value["assignees"]
            .as_array()
            .into_iter()
            .flatten()
            .map(user)
            .collect(),
        milestone: milestone(&value["milestone"]),
        linked_issues: parts
            .closes
            .iter()
            .map(|issue| link(issue, ItemKind::Issue))
            .collect(),
        created: timestamp(value, "created_at"),
        pinned_head: head.clone(),
        fingerprint: fingerprint(value),
        projects: Vec::new(),
        subscribed: value["subscribed"].as_bool(),
        reactions: Vec::new(),
        row,
    }
}

fn fingerprint(value: &Value) -> Fingerprint {
    Fingerprint {
        state: text(value, "state"),
        mergeable: text(value, "detailed_merge_status"),
        head: text(value, "sha"),
        rollup: value["head_pipeline"]["status"]
            .as_str()
            .map(pipeline_state),
        updated: timestamp(value, "updated_at"),
    }
}

fn note_position(note: &Value) -> Option<CommentPosition> {
    let position = &note["position"];
    if !position.is_object() {
        return None;
    }
    let line = position["new_line"]
        .as_u64()
        .or_else(|| position["old_line"].as_u64());
    Some(CommentPosition {
        path: optional(position, "new_path").unwrap_or_else(|| text(position, "old_path")),
        line,
        original_line: line,
        commit: optional(position, "head_sha"),
    })
}

fn note(value: &Value) -> Comment {
    Comment {
        id: id_of(value, "id"),
        author: user(&value["author"]),
        body: text(value, "body"),
        created: timestamp(value, "created_at"),
        system: value["system"].as_bool().unwrap_or(false),
        pending: false,
        position: note_position(value),
        association: None,
        // Resolving a discussion touches `updated_at` too; that is not an edit.
        edited: edited(value, "created_at", "updated_at")
            .filter(|at| Some(at.as_str()) != value["resolved_at"].as_str()),
        reactions: Vec::new(),
    }
}

/// A diff discussion as a review thread; its id names the merge request too, because GitLab
/// resolves discussions under `merge_requests/{iid}`.
fn thread(number: u64, discussion: &Value) -> Option<ReviewThread> {
    let notes = discussion["notes"].as_array()?;
    let first = notes.first()?;
    let position = &first["position"];
    if !position.is_object() {
        return None;
    }
    let left = position["new_line"].is_null() && position["old_line"].is_u64();
    let resolvable = notes
        .iter()
        .any(|note| note["resolvable"].as_bool() == Some(true));
    let comments = notes
        .iter()
        .filter(|note| note["system"].as_bool() != Some(true))
        .map(note)
        .collect::<Vec<_>>();
    let anchor = note_position(first)?;
    Some(ReviewThread {
        id: ItemId::new(format!("{number}/{}", text(discussion, "id"))),
        resolved: resolvable.then(|| {
            notes
                .iter()
                .filter(|note| note["resolvable"].as_bool() == Some(true))
                .all(|note| note["resolved"].as_bool() == Some(true))
        }),
        resolvable,
        path: Some(anchor.path.clone()),
        line: anchor.line,
        original_line: anchor.original_line,
        side: if left { Side::Left } else { Side::Right },
        commit: anchor.commit,
        comments,
        total_comments: None,
        // Discussions carry a position, not the diff lines around it.
        diff_hunk: None,
        start_line: None,
    })
}

/// Notes as the conversation: comments, approvals and GitLab's system notes ("added 1 commit",
/// "requested review from @alex", "mentioned in !5"). Diff notes live in the threads, not here.
fn timeline_events(value: &Value) -> Vec<TimelineEvent> {
    if value["type"] == "DiffNote" {
        return Vec::new();
    }
    let comment = note(value);
    if !comment.system {
        return vec![TimelineEvent::Commented(comment)];
    }
    let by = Actor {
        actor: comment.author.clone(),
        at: comment.created.clone(),
    };
    let body = comment.body.lines().next().unwrap_or_default().trim();
    let state = match body {
        "approved this merge request" => Some(ReviewState::Approved),
        "requested changes" => Some(ReviewState::ChangesRequested),
        _ => None,
    };
    if let Some(state) = state {
        return vec![TimelineEvent::Reviewed(Review {
            id: comment.id,
            author: comment.author,
            state,
            body: String::new(),
            submitted: comment.created,
        })];
    }
    let mentions = |rest: &str| -> Vec<String> {
        rest.split_whitespace()
            .filter_map(|word| word.strip_prefix('@'))
            .map(|login| login.trim_end_matches([',', '.']).to_owned())
            .filter(|login| !login.is_empty())
            .collect()
    };
    if body == "closed" {
        return vec![TimelineEvent::Closed { by }];
    }
    if body == "reopened" {
        return vec![TimelineEvent::Reopened { by }];
    }
    if let Some(rest) = body.strip_prefix("requested review from ") {
        return mentions(rest)
            .into_iter()
            .map(|reviewer| TimelineEvent::ReviewRequested {
                reviewer,
                by: by.clone(),
            })
            .collect();
    }
    if let Some(rest) = body.strip_prefix("assigned to ")
        && !rest.contains("unassigned")
    {
        return mentions(rest)
            .into_iter()
            .map(|assignee| TimelineEvent::Assigned {
                assignee,
                by: by.clone(),
            })
            .collect();
    }
    // "added 2 commits" lists them as `<li>1b9cc6c3 - Bound retry exponent</li>`.
    if body.starts_with("added ") && (body.ends_with(" commit") || body.ends_with(" commits")) {
        let commits = comment
            .body
            .split("<li>")
            .skip(1)
            .filter_map(|item| {
                let item = item.split("</li>").next()?;
                let (sha, headline) = item.split_once(" - ")?;
                Some(TimelineEvent::Committed(CommitSummary {
                    sha: sha.trim().to_owned(),
                    headline: headline.trim().to_owned(),
                    body: headline.trim().to_owned(),
                    committed: comment.created.clone(),
                    author: Some(comment.author.clone()),
                }))
            })
            .collect::<Vec<_>>();
        if !commits.is_empty() {
            return commits;
        }
    }
    // "mentioned in merge request !13" / "mentioned in issue #14" (same project).
    if let Some(rest) = body.strip_prefix("mentioned in ") {
        let (kind, reference) = match rest.split_once(' ') {
            Some(("merge", rest)) => (ItemKind::PullRequest, rest.trim_start_matches("request ")),
            Some(("issue", rest)) => (ItemKind::Issue, rest),
            _ => (ItemKind::Issue, ""),
        };
        let number = reference
            .trim_start_matches(['!', '#'])
            .parse::<u64>()
            .ok();
        if let Some(number) = number.filter(|_| !reference.contains('/')) {
            return vec![TimelineEvent::CrossReferenced {
                link: LinkRef {
                    number,
                    kind: Some(kind),
                    ..LinkRef::default()
                },
                by,
            }];
        }
    }
    vec![TimelineEvent::Other {
        kind: body.chars().take(120).collect(),
        by,
    }]
}

/// `resource_label_events` / `resource_milestone_events` entries: GitLab records label and
/// milestone changes there, not as notes.
fn resource_event(value: &Value) -> Option<TimelineEvent> {
    let by = Actor {
        actor: user(&value["user"]),
        at: timestamp(value, "created_at"),
    };
    let add = value["action"] == "add";
    if value["label"].is_object() {
        let label = text(&value["label"], "name");
        return Some(if add {
            TimelineEvent::Labeled { label, by }
        } else {
            TimelineEvent::Unlabeled { label, by }
        });
    }
    if value["milestone"].is_object() {
        let milestone = text(&value["milestone"], "title");
        return Some(if add {
            TimelineEvent::Milestoned { milestone, by }
        } else {
            TimelineEvent::Demilestoned { milestone, by }
        });
    }
    None
}

fn event_time(event: &TimelineEvent) -> Option<i64> {
    let at = match event {
        TimelineEvent::Commented(comment) => &comment.created,
        TimelineEvent::Reviewed(review) => &review.submitted,
        TimelineEvent::Committed(commit) => &commit.committed,
        TimelineEvent::Labeled { by, .. }
        | TimelineEvent::Unlabeled { by, .. }
        | TimelineEvent::Assigned { by, .. }
        | TimelineEvent::Unassigned { by, .. }
        | TimelineEvent::Closed { by }
        | TimelineEvent::Reopened { by }
        | TimelineEvent::Merged { by, .. }
        | TimelineEvent::ForcePushed { by, .. }
        | TimelineEvent::CrossReferenced { by, .. }
        | TimelineEvent::Renamed { by, .. }
        | TimelineEvent::ReviewRequested { by, .. }
        | TimelineEvent::Milestoned { by, .. }
        | TimelineEvent::Demilestoned { by, .. }
        | TimelineEvent::Other { by, .. }
        | TimelineEvent::Detailed { by, .. } => &by.at,
    };
    crate::forge::records::epoch(at.as_str())
}

fn pipeline_run(value: &Value) -> Run {
    let status = text(value, "status");
    let conclusion = conclusion_word(&status);
    let reference = text(value, "ref");
    Run {
        id: id_of(value, "id"),
        number: count(value, "iid"),
        title: optional(value, "name").unwrap_or_else(|| {
            let source = text(value, "source").replace('_', " ");
            if source.is_empty() {
                reference.clone()
            } else {
                format!("{reference} · {source}")
            }
        }),
        workflow: optional(value, "source").map(|source| source_workflow(&source)),
        status: run_status(&status),
        conclusion: conclusion.map(|_| pipeline_state(&status)),
        conclusion_raw: conclusion.map(str::to_owned),
        pr_number: reference
            .strip_prefix("refs/merge-requests/")
            .and_then(|rest| rest.split('/').next())
            .and_then(|number| number.parse().ok()),
        branch: reference,
        sha: text(value, "sha"),
        event: text(value, "source"),
        // A retried pipeline starts after it was created; a queued one has no start yet.
        created: optional_time(value, "started_at")
            .or_else(|| optional_time(value, "created_at"))
            .unwrap_or_default(),
        duration: value["duration"].as_f64().map(Duration::from_secs_f64),
        attempt: 1,
        attempts: 1,
        web_url: text(value, "web_url"),
        needs_approval: false,
        actor: value["user"]["username"].as_str().unwrap_or_default().to_owned(),
    }
}

fn pipeline_job(value: &Value) -> Job {
    let status = text(value, "status");
    let conclusion = conclusion_word(&status).map(|word| match word {
        // A manual job waits for a person; it is not a failed or required step.
        "action_required" => "skipped",
        word => word,
    });
    Job {
        id: id_of(value, "id"),
        name: text(value, "name"),
        status: run_status(&status),
        conclusion: conclusion.map(|_| job_state(value)),
        conclusion_raw: conclusion.map(str::to_owned),
        started: optional_time(value, "started_at"),
        completed: optional_time(value, "finished_at"),
        steps: Steps::FromLogSections,
        web_url: optional(value, "web_url")
            .or_else(|| optional(&value["downstream_pipeline"], "web_url"))
            .unwrap_or_default(),
        stage: optional(value, "stage"),
        retried: value["retried"].as_bool().unwrap_or(false),
    }
}

/// A pipeline source as the workflows rail shows it (one `.gitlab-ci.yml` per project; the
/// rail filters by what started the pipeline).
fn source_workflow(source: &str) -> Workflow {
    let name = match source {
        "push" => "Push",
        "merge_request_event" => "Merge request",
        "schedule" => "Schedule",
        "web" => "Run pipeline",
        "api" => "API",
        "trigger" => "Trigger",
        "pipeline" => "Multi-project",
        "parent_pipeline" => "Child pipeline",
        "external_pull_request_event" => "External pull request",
        "chat" => "ChatOps",
        "security_orchestration_policy" => "Security policy",
        other => other,
    };
    Workflow {
        id: ItemId::new(source),
        name: name.to_owned(),
        path: ".gitlab-ci.yml".into(),
        state: "active".into(),
        dispatchable: Some(source == "web"),
    }
}

/// A schedule's workflow id: `schedule:<id>`, apart from the `schedule` source.
const SCHEDULE: &str = "schedule:";

/// A pipeline schedule as a row of the Workflows panel: its description, muted `(off)` when
/// inactive.
fn schedule_workflow(value: &Value) -> Workflow {
    let id = id_of(value, "id").0;
    Workflow {
        id: ItemId::new(format!("{SCHEDULE}{id}")),
        name: optional(value, "description").unwrap_or_else(|| format!("Schedule {id}")),
        path: ".gitlab-ci.yml".into(),
        state: if value["active"].as_bool().unwrap_or(true) { "active" } else { "disabled_manually" }.into(),
        dispatchable: Some(false),
    }
}

/// GitHub's run status words (`RunFilter::status`) in GitLab's vocabulary.
fn pipeline_status_word(status: &str) -> &str {
    match status {
        "in_progress" => "running",
        "queued" => "pending",
        "failure" => "failed",
        "cancelled" => "canceled",
        "action_required" => "manual",
        "waiting" => "waiting_for_resource",
        other => other,
    }
}

/// `refs/merge-requests/N/head` → the merge request's source branch, read once per process.
fn mr_branches() -> &'static Mutex<HashMap<String, String>> {
    static BRANCHES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    BRANCHES.get_or_init(Default::default)
}

/// A job's archive as an artifact (`artifacts_file`, or the `archive` entry of `artifacts`).
fn job_artifact(job: &Value) -> Option<Artifact> {
    let archive = job["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|file| file["file_type"] == "archive");
    let size = job["artifacts_file"]["size"]
        .as_u64()
        .or_else(|| archive.and_then(|file| file["size"].as_u64()))?;
    if job["artifacts_file"].is_null() && archive.is_none() {
        return None;
    }
    let web = text(job, "web_url");
    Some(Artifact {
        id: id_of(job, "id"),
        name: text(job, "name"),
        size,
        expires: optional_time(job, "artifacts_expire_at"),
        download: if web.is_empty() {
            ArtifactSource::Cli
        } else {
            ArtifactSource::Url(format!("{web}/artifacts/download"))
        },
    })
}

/// Retried jobs come back beside their retries (`include_retried`); keep the newest of each.
fn latest_jobs(jobs: Vec<Value>) -> Vec<Value> {
    let mut kept: Vec<Value> = Vec::new();
    for job in jobs {
        let key = (text(&job, "stage"), text(&job, "name"));
        match kept
            .iter_mut()
            .find(|kept| (text(kept, "stage"), text(kept, "name")) == key)
        {
            Some(existing) => {
                if count(&job, "id") > count(existing, "id") {
                    *existing = job;
                }
            }
            None => kept.push(job),
        }
    }
    kept
}

fn todo(value: &Value) -> Notification {
    let target = &value["target"];
    let number = count(target, "iid");
    let target_kind = match value["target_type"].as_str() {
        Some("MergeRequest") if number > 0 => NotifyTarget::Item {
            kind: ItemKind::PullRequest,
            number,
        },
        Some("Issue") if number > 0 => NotifyTarget::Item {
            kind: ItemKind::Issue,
            number,
        },
        Some("Commit") | Some("Pipeline") => {
            NotifyTarget::Run(ItemId::new(text(value, "target_url")))
        }
        kind => NotifyTarget::Other(kind.unwrap_or_default().into()),
    };
    Notification {
        id: id_of(value, "id"),
        title: optional(target, "title").unwrap_or_else(|| text(value, "body")),
        reason: match value["action_name"].as_str().unwrap_or_default() {
            "mentioned" | "directly_addressed" => NotifyReason::Mentioned,
            "review_requested" | "approval_required" => NotifyReason::ReviewRequested,
            "assigned" => NotifyReason::Assigned,
            "build_failed" => NotifyReason::CiActivity,
            "unmergeable" | "merge_train_removed" => NotifyReason::StateChange,
            "marked" => NotifyReason::Subscribed,
            "review_submitted" => NotifyReason::Comment,
            other => NotifyReason::Other(other.into()),
        },
        unread: value["state"].as_str() != Some("done"),
        repo: text(&value["project"], "path_with_namespace"),
        target: target_kind,
        updated: optional_time(value, "updated_at")
            .unwrap_or_else(|| timestamp(value, "created_at")),
        comment: None,
    }
}

fn repo_payload(value: &Value) -> Repo {
    let level = |key: &str| {
        value["permissions"][key]["access_level"]
            .as_u64()
            .unwrap_or(0)
    };
    let access = level("project_access").max(level("group_access"));
    let merge_method = text(value, "merge_method");
    let squash = text(value, "squash_option");
    let mut merge_methods = vec![if merge_method == "ff" {
        MergeMethod::FastForwardOnly
    } else {
        MergeMethod::Merge
    }];
    if squash != "never" {
        merge_methods.push(MergeMethod::Squash);
    }
    merge_methods.push(MergeMethod::Rebase);
    let parent = &value["forked_from_project"];
    Repo {
        id: id_of(value, "id"),
        full_name: text(value, "path_with_namespace"),
        default_branch: text(value, "default_branch"),
        web_url: text(value, "web_url"),
        is_fork: parent.is_object(),
        parent: optional(parent, "path_with_namespace"),
        parent_default_branch: optional(parent, "default_branch"),
        permissions: Permissions {
            admin: access >= 40,
            push: access >= 30,
            pull: true,
        },
        merge_methods,
        default_merge_method: Some(if matches!(squash.as_str(), "always" | "default_on") {
            MergeMethod::Squash
        } else {
            MergeMethod::Merge
        }),
        delete_branch_on_merge: value["remove_source_branch_after_merge"]
            .as_bool()
            .unwrap_or(false),
        has_ci: value["jobs_enabled"].as_bool().unwrap_or(true)
            && value["builds_access_level"].as_str() != Some("disabled"),
        pr_template: None,
    }
}

// ------------------------------------------------------------------------------ query translation

/// GitHub search syntax (what the UI and saved views speak) as GitLab filters (§3.6).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub text: Vec<String>,
    pub author: Option<String>,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub approved_by: Option<String>,
    pub labels: Vec<String>,
    pub not_labels: Vec<String>,
    pub milestone: Option<String>,
    pub draft: Option<bool>,
    pub state: Option<StateFilter>,
    pub sort: Option<&'static str>,
    pub repo: Option<String>,
    pub group: Option<String>,
    /// `no:label`, `no:assignee`, `no:milestone`: GitLab's `None` wildcards.
    pub no: Vec<String>,
    /// A `sort:` GitLab has no order for (`comments-desc`): the returned rows are sorted.
    pub client_sort: Option<String>,
}

fn words(query: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for character in query.chars() {
        match character {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

impl Query {
    pub fn parse(query: &str) -> Self {
        let mut parsed = Query::default();
        for word in words(query) {
            let Some((key, value)) = word.split_once(':') else {
                parsed.text.push(word);
                continue;
            };
            let value = value.to_owned();
            match key.to_ascii_lowercase().as_str() {
                "author" => parsed.author = Some(value),
                "assignee" => parsed.assignee = Some(value),
                "review-requested" | "reviewer" => parsed.reviewer = Some(value),
                "reviewed-by" | "approved-by" => parsed.approved_by = Some(value),
                "label" => parsed.labels.extend(value.split(',').map(str::to_owned)),
                "-label" => parsed
                    .not_labels
                    .extend(value.split(',').map(str::to_owned)),
                "milestone" => parsed.milestone = Some(value),
                "draft" => parsed.draft = Some(value != "false"),
                "is" | "state" => match value.to_ascii_lowercase().as_str() {
                    "draft" => parsed.draft = Some(true),
                    "open" | "opened" => parsed.state = Some(StateFilter::Open),
                    "closed" => parsed.state = Some(StateFilter::Closed),
                    "merged" => parsed.state = Some(StateFilter::Merged),
                    "all" => parsed.state = Some(StateFilter::All),
                    // is:pr / is:issue / is:unmerged say nothing GitLab needs.
                    _ => {}
                },
                "repo" | "project" => parsed.repo = Some(value),
                "org" | "group" | "user" => parsed.group = Some(value),
                "sort" => {
                    if value.starts_with("comments-") {
                        parsed.client_sort = Some(value.clone());
                    }
                    parsed.sort = Some(match value.as_str() {
                        "created-desc" => "CREATED_DESC",
                        "created-asc" => "CREATED_ASC",
                        "updated-asc" => "UPDATED_ASC",
                        _ => "UPDATED_DESC",
                    })
                }
                "no" => parsed.no.push(value.to_ascii_lowercase()),
                // `mentions:` has no GitLab filter; the rest is text GitLab can search.
                "mentions" => {}
                _ => parsed.text.push(word),
            }
        }
        parsed
    }

    pub fn mentions_me(&self) -> bool {
        [
            &self.author,
            &self.assignee,
            &self.reviewer,
            &self.approved_by,
        ]
        .into_iter()
        .flatten()
        .any(|who| who.eq_ignore_ascii_case("@me"))
    }

    fn who(value: &Option<String>, me: Option<&str>) -> Option<String> {
        let value = value.as_deref()?;
        if value.eq_ignore_ascii_case("@me") {
            me.map(str::to_owned)
        } else {
            Some(value.trim_start_matches('@').to_owned())
        }
    }
}

#[derive(Default)]
struct Arguments {
    declarations: String,
    arguments: String,
    variables: serde_json::Map<String, Value>,
}

impl Arguments {
    fn add(&mut self, name: &str, kind: &str, value: Value) {
        self.declarations.push_str(&format!(", ${name}: {kind}"));
        self.arguments.push_str(&format!(", {name}: ${name}"));
        self.variables.insert(name.into(), value);
    }
}

/// The list document and its variables: only the arguments a filter uses are declared, so
/// older servers that lack one (`search` on merge requests) only fail when it is used.
pub fn list_document(
    kind: ItemKind,
    path: &str,
    filter: &Filter,
    me: Option<&str>,
) -> (String, Value) {
    let query = Query::parse(&filter.query);
    let merge_requests = kind == ItemKind::PullRequest;
    let mut args = Arguments::default();
    let state = match (query.state.unwrap_or(filter.state), merge_requests) {
        (StateFilter::Open, _) => "opened",
        (StateFilter::Merged, true) => "merged",
        (StateFilter::Closed | StateFilter::Merged, _) => "closed",
        (StateFilter::All, _) => "all",
    };
    args.add(
        "state",
        if merge_requests {
            "MergeRequestState"
        } else {
            "IssuableState"
        },
        state.into(),
    );
    args.add(
        "sort",
        if merge_requests {
            "MergeRequestSort"
        } else {
            "IssueSort"
        },
        query.sort.unwrap_or("UPDATED_DESC").into(),
    );
    if !query.text.is_empty() {
        args.add("search", "String", query.text.join(" ").into());
    }
    if let Some(author) = Query::who(&query.author, me) {
        args.add("authorUsername", "String", author.into());
    }
    if let Some(assignee) = Query::who(&query.assignee, me) {
        args.add("assigneeUsername", "String", assignee.into());
    }
    if merge_requests {
        if let Some(reviewer) = Query::who(&query.reviewer, me) {
            args.add("reviewerUsername", "String", reviewer.into());
        }
        if let Some(approver) = Query::who(&query.approved_by, me) {
            args.add("approvedBy", "[String!]", json!([approver]));
        }
        if let Some(draft) = query.draft {
            args.add("draft", "Boolean", draft.into());
        }
    }
    let (labels, labels_type, negated) = if merge_requests {
        ("labels", "[String!]", "MergeRequestsResolverNegatedParams")
    } else {
        ("labelName", "[String]", "NegatedIssueFilterInput")
    };
    if !query.labels.is_empty() {
        args.add(labels, labels_type, json!(query.labels));
    }
    if let Some(milestone) = &query.milestone {
        if merge_requests {
            args.add("milestoneTitle", "String", milestone.clone().into());
        } else {
            args.add("milestoneTitle", "[String]", json!([milestone]));
        }
    }
    if !query.not_labels.is_empty() {
        args.add("not", negated, json!({ labels: query.not_labels }));
    }
    for wildcard in &query.no {
        match wildcard.as_str() {
            "label" if query.labels.is_empty() => args.add(labels, labels_type, json!(["None"])),
            "assignee" => args.add("assigneeWildcardId", "AssigneeWildcardId", "NONE".into()),
            "milestone" => args.add("milestoneWildcardId", "MilestoneWildcardId", "NONE".into()),
            _ => {}
        }
    }
    let (operation, connection, fields) = if merge_requests {
        ("MergeRequestList", "mergeRequests", MR_ROW_FIELDS)
    } else {
        ("IssueList", "issues", ISSUE_ROW_FIELDS)
    };
    let document = format!(
        "query {operation}($path: ID!, $first: Int!, $after: String{declarations}) {{ project(fullPath: $path) {{ \
{connection}(first: $first, after: $after{arguments}) {{ count pageInfo {{ hasNextPage endCursor }} nodes {{ {fields} }} }} }} }}",
        declarations = args.declarations,
        arguments = args.arguments,
    );
    let mut variables = args.variables;
    variables.insert("path".into(), path.into());
    (document, Value::Object(variables))
}

/// Host-wide search (`GET /merge_requests?scope=all`, the inbox) or one project or group.
pub fn search_request(kind: ItemKind, filter: &Filter, page: Page, me: Option<&str>) -> ApiRequest {
    let query = Query::parse(&filter.query);
    let merge_requests = kind == ItemKind::PullRequest;
    let collection = if merge_requests {
        "merge_requests"
    } else {
        "issues"
    };
    let path = match (&query.repo, &query.group) {
        (Some(repo), _) => format!("projects/{}/{collection}", path_segment(repo)),
        (None, Some(group)) => format!("groups/{}/{collection}", path_segment(group)),
        (None, None) => collection.to_owned(),
    };
    let mut request = ApiRequest::get(path).query("scope", "all");
    let state = match (query.state.unwrap_or(filter.state), merge_requests) {
        (StateFilter::Open, _) => Some("opened"),
        (StateFilter::Merged, true) => Some("merged"),
        (StateFilter::Closed | StateFilter::Merged, _) => Some("closed"),
        (StateFilter::All, _) => None,
    };
    if let Some(state) = state {
        request = request.query("state", state);
    }
    if !query.text.is_empty() {
        request = request.query("search", query.text.join(" "));
    }
    if let Some(author) = Query::who(&query.author, me) {
        request = request.query("author_username", author);
    }
    if let Some(assignee) = Query::who(&query.assignee, me) {
        request = request.query("assignee_username", assignee);
    }
    if merge_requests {
        if let Some(reviewer) = Query::who(&query.reviewer, me) {
            request = request.query("reviewer_username", reviewer);
        }
        if let Some(approver) = Query::who(&query.approved_by, me) {
            request = request.query("approved_by_usernames[]", approver);
        }
        if let Some(draft) = query.draft {
            request = request.query("wip", if draft { "yes" } else { "no" });
        }
    }
    if !query.labels.is_empty() {
        request = request.query("labels", query.labels.join(","));
    }
    if !query.not_labels.is_empty() {
        request = request.query("not[labels]", query.not_labels.join(","));
    }
    if let Some(milestone) = &query.milestone {
        request = request.query("milestone", milestone);
    }
    // `no:label` and friends: the REST `None` wildcards.
    for wildcard in &query.no {
        request = match wildcard.as_str() {
            "label" if query.labels.is_empty() => request.query("labels", "None"),
            "assignee" => request.query("assignee_id", "None"),
            "milestone" => request.query("milestone", "None"),
            _ => request,
        };
    }
    let (order, sort) = match query.sort {
        Some("CREATED_DESC") => ("created_at", "desc"),
        Some("CREATED_ASC") => ("created_at", "asc"),
        Some("UPDATED_ASC") => ("updated_at", "asc"),
        _ => ("updated_at", "desc"),
    };
    request
        .query("order_by", order)
        .query("sort", sort)
        .query("per_page", page.per_page.clamp(1, PAGE))
        .query("page", page.number.max(1))
}

// ------------------------------------------------------------------------------ process memos

/// `@me` per host: one `GET user` per process.
fn viewers() -> &'static Mutex<HashMap<String, String>> {
    static VIEWERS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    VIEWERS.get_or_init(Default::default)
}

/// GitLab numbers issues and merge requests separately, but `Read::Timeline(n)` names no kind:
/// the timeline follows the item last read under that number.
/// (host, repository, number) → the kind last read under that number.
type Kinds = HashMap<(String, String, u64), ItemKind>;

fn kinds() -> &'static Mutex<Kinds> {
    static KINDS: OnceLock<Mutex<Kinds>> = OnceLock::new();
    KINDS.get_or_init(Default::default)
}

/// Probed capabilities (request changes, draft notes) per host, mirrored into `hosts.json`.
fn probes() -> &'static Mutex<HashMap<(String, &'static str), bool>> {
    static PROBES: OnceLock<Mutex<HashMap<(String, &'static str), bool>>> = OnceLock::new();
    PROBES.get_or_init(Default::default)
}

fn lock<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn kind_key(ctx: &ForgeContext, number: u64) -> (String, String, u64) {
    (
        ctx.host().to_string(),
        ctx.repo
            .as_ref()
            .map(super::RepoRef::full)
            .unwrap_or_default(),
        number,
    )
}

fn remember(ctx: &ForgeContext, number: u64, kind: ItemKind) {
    lock(kinds()).insert(kind_key(ctx, number), kind);
}

fn recall(ctx: &ForgeContext, number: u64) -> ItemKind {
    lock(kinds())
        .get(&kind_key(ctx, number))
        .copied()
        .unwrap_or(ItemKind::PullRequest)
}

/// What the screen knows the item under `number` is (the tab it reads it from), for the
/// timeline read that names only the number.
pub fn note_kind(ctx: &ForgeContext, number: u64, kind: ItemKind) {
    remember(ctx, number, kind);
}

/// After an item read: its kind kept in the disk cache (`item_kind`), so a timeline read
/// after a restart follows the kind read last time instead of guessing.
pub fn keep_kind(ctx: &ForgeContext, cache: &crate::forge::cache::Cache, number: u64) {
    keep_in(cache, kind_key(ctx, number));
}

/// Before a timeline read: the kind kept on disk, when this process has not read the item.
pub fn restore_kind(ctx: &ForgeContext, cache: &crate::forge::cache::Cache, number: u64) {
    restore_in(cache, kind_key(ctx, number));
}

const KIND_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

fn kind_entry(key: &(String, String, u64)) -> crate::forge::cache::CacheKey {
    let repo = Some(key.1.as_str()).filter(|repo| !repo.is_empty());
    crate::forge::cache::CacheKey::named(&key.0, repo, "item_kind", &key.2.to_string())
}

fn kind_name(kind: ItemKind) -> &'static str {
    if kind == ItemKind::Issue { "issue" } else { "pull_request" }
}

fn keep_in(cache: &crate::forge::cache::Cache, key: (String, String, u64)) {
    let Some(kind) = lock(kinds()).get(&key).copied() else {
        return;
    };
    let entry = kind_entry(&key);
    let kept = cache.get::<String>(&entry).map(|envelope| envelope.data);
    if kept.as_deref() != Some(kind_name(kind)) {
        cache.store(&entry, kind_name(kind), KIND_TTL, None, None);
    }
}

fn restore_in(cache: &crate::forge::cache::Cache, key: (String, String, u64)) {
    if lock(kinds()).contains_key(&key) {
        return;
    }
    if let Some(envelope) = cache.get::<String>(&kind_entry(&key)) {
        let kind = if envelope.data == "issue" { ItemKind::Issue } else { ItemKind::PullRequest };
        lock(kinds()).insert(key, kind);
    }
}

fn observed(ctx: &ForgeContext, capability: &'static str) -> Option<bool> {
    if let Some(known) = lock(probes())
        .get(&(ctx.host().to_string(), capability))
        .copied()
    {
        return Some(known);
    }
    crate::forge::detect::observed_capabilities(ctx.host())
        .get(capability)
        .map(|word| word == "yes")
}

fn record(ctx: &ForgeContext, capability: &'static str, supported: bool) {
    lock(probes()).insert((ctx.host().to_string(), capability), supported);
    crate::forge::detect::record_capability(ctx.host(), &ctx.endpoints, capability, supported);
}

/// `$XDG_STATE_HOME/dusty/forge/viewed/<host>/<project>/<iid>.json` (FORGE.md §4.2): GitLab
/// keeps viewed files in the browser only.
fn viewed_path(ctx: &ForgeContext, number: u64) -> Option<PathBuf> {
    let hosts = crate::forge::detect::hosts_path(&crate::forge::credential::Env::process())?;
    let repo = ctx.repo.as_ref()?;
    Some(
        hosts
            .parent()?
            .join("viewed")
            .join(path_segment(&ctx.host().to_string()))
            .join(path_segment(&repo.full()))
            .join(format!("{number}.json")),
    )
}

fn read_viewed(ctx: &ForgeContext, number: u64) -> Vec<(String, bool)> {
    let Some(path) = viewed_path(ctx, number) else {
        return Vec::new();
    };
    let value: Value = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    value["files"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(path, viewed)| (path.clone(), viewed.as_bool().unwrap_or(false)))
        .collect()
}

fn write_viewed(
    ctx: &ForgeContext,
    number: u64,
    path: &str,
    viewed: bool,
) -> Result<(), ForgeError> {
    let file = viewed_path(ctx, number)
        .ok_or_else(|| ForgeError::Failed("no state directory for viewed files".into()))?;
    let mut files = serde_json::Map::new();
    for (name, seen) in read_viewed(ctx, number) {
        files.insert(name, seen.into());
    }
    if viewed {
        files.insert(path.into(), true.into());
    } else {
        files.remove(path);
    }
    let parent = file
        .parent()
        .ok_or_else(|| ForgeError::Failed("invalid viewed-files path".into()))?;
    std::fs::create_dir_all(parent).map_err(|error| ForgeError::Failed(error.to_string()))?;
    let temporary = file.with_extension("json.tmp");
    std::fs::write(&temporary, json!({ "files": files }).to_string())
        .and_then(|()| std::fs::rename(&temporary, &file))
        .map_err(|error| ForgeError::Failed(error.to_string()))
}

// ------------------------------------------------------------------------------ transport helpers

/// GraphQL's 200-with-errors: missing data becomes an error, partial data is kept.
fn graphql_data(body: Value) -> Result<Value, ForgeError> {
    let errors = body["errors"].as_array().cloned().unwrap_or_default();
    let message = crate::forge::cli::redact(
        errors
            .iter()
            .find_map(|error| error["message"].as_str())
            .unwrap_or_default(),
    );
    let data = body["data"].clone();
    if data.is_null()
        || (data
            .as_object()
            .is_some_and(|object| object.values().all(Value::is_null))
            && !errors.is_empty())
    {
        let lower = message.to_ascii_lowercase();
        return Err(if errors.is_empty() {
            ForgeError::BadResponse("GraphQL returned no data".into())
        } else if lower.contains("rate limit") || lower.contains("too many requests") {
            ForgeError::RateLimited {
                until: std::time::Instant::now() + Duration::from_secs(60),
            }
        } else if lower.contains("does not exist") || lower.contains("not found") {
            ForgeError::NotFound(message)
        } else if lower.contains("permission") {
            ForgeError::Forbidden(message)
        } else {
            ForgeError::Failed(message)
        });
    }
    Ok(data)
}

/// The next page of a REST list: `x-next-page`, else a keyset `Link: rel="next"`.
fn next_page(response: &ApiResponse) -> Option<NextPage> {
    if let Some(page) = response
        .header("x-next-page")
        .map(str::trim)
        .filter(|page| !page.is_empty())
    {
        return Some(NextPage::Number(page.to_owned()));
    }
    response.next_link().map(NextPage::Link)
}

enum NextPage {
    Number(String),
    Link(String),
}

fn with_page(mut request: ApiRequest, page: impl ToString) -> ApiRequest {
    request.query.retain(|(key, _)| key != "page");
    request.query.push(("page".into(), page.to_string()));
    request
}

fn total(response: &ApiResponse) -> Option<u64> {
    response
        .header("x-total")
        .and_then(|total| total.trim().parse().ok())
}

fn conflict(error: ForgeError, message: &str) -> ForgeError {
    match error {
        ForgeError::Conflict(detail) | ForgeError::Failed(detail)
            if detail.contains("SHA does not match") =>
        {
            ForgeError::Conflict(message.into())
        }
        ForgeError::Failed(detail) if detail.starts_with("406") || detail.starts_with("405") => {
            ForgeError::Conflict(
                detail
                    .split_once(' ')
                    .map(|(_, message)| message)
                    .unwrap_or_default()
                    .into(),
            )
        }
        error => error,
    }
}

const MOVED: &str = "the merge request moved since you reviewed it · reload to see the new commits";

impl GitLab {
    fn project(ctx: &ForgeContext, tail: &str) -> Result<String, ForgeError> {
        let id = path_segment(&ctx.repo()?.full());
        Ok(if tail.is_empty() {
            format!("projects/{id}")
        } else {
            format!("projects/{id}/{tail}")
        })
    }

    fn graphql(
        ctx: &ForgeContext,
        document: &str,
        variables: Value,
        cancel: &AtomicBool,
    ) -> Result<Value, ForgeError> {
        let response = ctx.send(&ApiRequest::graphql(document, variables), cancel)?;
        if !response.success() {
            return Err(crate::forge::http::classify(&ctx.endpoints, &response));
        }
        graphql_data(response.json()?)
    }

    /// Every page of a REST list (bounded), and `x-total` when GitLab counted.
    fn rest_pages(
        ctx: &ForgeContext,
        first: ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<(Vec<Value>, Option<u64>), ForgeError> {
        let mut request = first;
        let mut all = Vec::new();
        let mut counted = None;
        for _ in 0..MAX_PAGES {
            if cancel.load(Ordering::Acquire) {
                return Err(ForgeError::Cancelled);
            }
            let response = ctx.ok(&request, cancel)?;
            counted = counted.or_else(|| total(&response));
            match response.json()? {
                Value::Array(items) => all.extend(items),
                Value::Null => {}
                other => all.push(other),
            }
            request = match next_page(&response) {
                Some(NextPage::Number(page)) => with_page(request, page),
                Some(NextPage::Link(url)) => ApiRequest {
                    target: link_target(ctx, &url),
                    query: Vec::new(),
                    ..request
                },
                None => break,
            };
        }
        Ok((all, counted))
    }

    /// The signed-in username for `@me` filters.
    fn me(ctx: &ForgeContext, cancel: &AtomicBool) -> Result<String, ForgeError> {
        let host = ctx.host().to_string();
        if let Some(login) = lock(viewers()).get(&host) {
            return Ok(login.clone());
        }
        if ctx.anonymous() {
            return Err(ctx.sign_in_to("filter by @me"));
        }
        let login = user(&ctx.json(&ApiRequest::get("user"), cancel)?).login;
        if login.is_empty() {
            return Err(ForgeError::BadResponse("GET user has no username".into()));
        }
        lock(viewers()).insert(host, login.clone());
        Ok(login)
    }

    /// A GraphQL list page: the cursors are walked to `page.number`, like GitHub's search.
    fn rows(
        ctx: &ForgeContext,
        kind: ItemKind,
        filter: &Filter,
        page: Page,
        cancel: &AtomicBool,
    ) -> Result<PageOf<ItemRow>, ForgeError> {
        let full = ctx.repo()?.full();
        let me = if Query::parse(&filter.query).mentions_me() {
            Some(Self::me(ctx, cancel)?)
        } else {
            None
        };
        let (document, mut variables) = list_document(kind, &full, filter, me.as_deref());
        let connection = match kind {
            ItemKind::PullRequest => "mergeRequests",
            ItemKind::Issue => "issues",
        };
        let skip = (page.number.max(1) as usize - 1) * page.per_page as usize;
        let wanted = skip + page.per_page.max(1) as usize;
        let mut items = Vec::new();
        let mut counted = None;
        let mut more = false;
        let mut after: Option<String> = None;
        while items.len() < wanted {
            if cancel.load(Ordering::Acquire) {
                return Err(ForgeError::Cancelled);
            }
            variables["first"] = ((wanted - items.len()).min(PAGE as usize)).into();
            variables["after"] = after.clone().map(Value::String).unwrap_or(Value::Null);
            let data = Self::graphql(ctx, &document, variables.clone(), cancel)?;
            let project = &data["project"];
            if !project.is_object() {
                return Err(ForgeError::NotFound(format!("project {full}")));
            }
            let list = &project[connection];
            counted = list["count"].as_u64();
            let page_nodes = nodes(list);
            items.extend(page_nodes.iter().map(|node| graphql_row(node, kind, &full)));
            more = list["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            after = list["pageInfo"]["endCursor"].as_str().map(str::to_owned);
            if !more || after.is_none() || page_nodes.is_empty() {
                break;
            }
        }
        Ok(PageOf {
            items: items
                .into_iter()
                .skip(skip)
                .take(page.per_page as usize)
                .collect(),
            next: more.then_some(Page {
                number: page.number + 1,
                per_page: page.per_page,
            }),
            total: counted,
            other_total: None,
        })
    }

    /// The first request of every Read, for `plan()` and `describe()`.
    fn requests(ctx: &ForgeContext, read: &Read) -> Result<Vec<ApiRequest>, ForgeError> {
        let rest = |tail: &str| Self::project(ctx, tail).map(ApiRequest::get);
        let paged = |tail: &str| rest(tail).map(|request| request.query("per_page", PAGE));
        Ok(match read {
            Read::Repository => vec![rest("")?],
            Read::RepositoryNamed(name) => {
                vec![ApiRequest::get(format!("projects/{}", path_segment(name)))]
            }
            Read::Viewer => vec![ApiRequest::get("user")],
            Read::Labels => vec![paged("labels")?],
            Read::Milestones => vec![paged("milestones")?.query("include_ancestors", "true")],
            Read::Collaborators => vec![paged("members/all")?],
            Read::List { kind, filter, .. } => {
                let (document, mut variables) =
                    list_document(*kind, &ctx.repo()?.full(), filter, None);
                variables["first"] = PAGE.into();
                vec![ApiRequest::graphql(&document, variables)]
            }
            Read::Search { kind, filter, page } => vec![search_request(*kind, filter, *page, None)],
            Read::PullRequest(number) => {
                let mr = format!("merge_requests/{number}");
                vec![
                    rest(&mr)?,
                    rest(&format!("{mr}/approvals"))?,
                    rest(&format!("{mr}/reviewers"))?,
                    paged(&format!("{mr}/commits"))?,
                    paged(&format!("{mr}/diffs"))?,
                    rest(&format!("{mr}/closes_issues"))?,
                    paged(&format!("{mr}/award_emoji"))?,
                ]
            }
            Read::Fingerprint(number) => vec![rest(&format!("merge_requests/{number}"))?],
            Read::Issue(number) => vec![
                rest(&format!("issues/{number}"))?,
                rest(&format!("issues/{number}/closed_by"))?,
                paged(&format!("issues/{number}/award_emoji"))?,
            ],
            Read::Timeline(number) => {
                let item = item_path(recall(ctx, *number), *number);
                vec![
                    paged(&format!("{item}/notes"))?
                        .query("sort", "asc")
                        .query("order_by", "created_at"),
                    paged(&format!("{item}/resource_label_events"))?,
                    paged(&format!("{item}/resource_milestone_events"))?,
                ]
            }
            Read::Threads(number) => vec![paged(&format!("merge_requests/{number}/discussions"))?],
            Read::Diff(number) => vec![paged(&format!("merge_requests/{number}/diffs"))?],
            Read::CommitDiff(sha) => {
                vec![paged(&format!("repository/commits/{}/diff", path_segment(sha)))?]
            }
            Read::Viewed(_) => Vec::new(),
            Read::Workflows => vec![
                rest("pipelines")?
                    .query("order_by", "updated_at")
                    .query("sort", "desc")
                    .query("per_page", PAGE),
                // The project's schedules are rows of their own (Developer role; else none).
                rest("pipeline_schedules")?.query("per_page", PAGE),
            ],
            Read::PullRequestForBranch(branch) => vec![
                rest("merge_requests")?
                    .query("source_branch", branch)
                    .query("state", "opened"),
            ],
            Read::WorkflowSource { reference, .. } => {
                let mut request = rest("repository/files/.gitlab-ci.yml/raw")?.accept(Accept::Raw);
                if let Some(reference) =
                    reference.as_ref().filter(|reference| !reference.is_empty())
                {
                    request = request.query("ref", reference);
                }
                vec![request]
            }
            Read::Runs { filter, page }
                if filter.workflow.as_ref().is_some_and(|workflow| {
                    crate::forge::legacy::workflow_id(workflow).0.starts_with(SCHEDULE)
                }) =>
            {
                // A schedule's row lists the pipelines that schedule started.
                let workflow = crate::forge::legacy::workflow_id(filter.workflow.as_ref().unwrap_or(&ItemId::new("")));
                let schedule = workflow.0.trim_start_matches(SCHEDULE);
                vec![
                    rest(&format!("pipeline_schedules/{}/pipelines", path_segment(schedule)))?
                        .query("per_page", page.per_page.clamp(1, PAGE))
                        .query("page", page.number.max(1)),
                ]
            }
            Read::Runs { filter, page } => {
                let mut request = rest("pipelines")?
                    .query("order_by", "updated_at")
                    .query("sort", "desc")
                    .query("per_page", page.per_page.clamp(1, PAGE))
                    .query("page", page.number.max(1));
                if let Some(branch) = &filter.branch {
                    request = request.query("ref", branch);
                }
                // The workflows rail is pipeline sources; an event filter says the same.
                let source = filter
                    .workflow
                    .as_ref()
                    .map(|workflow| crate::forge::legacy::workflow_id(workflow).0)
                    .or_else(|| filter.event.clone());
                if let Some(source) = source.filter(|source| !source.is_empty()) {
                    request = request.query("source", source);
                }
                if let Some(status) = &filter.status {
                    request = request.query("status", pipeline_status_word(status));
                }
                vec![request]
            }
            Read::Run { id, .. } => vec![
                rest(&format!("pipelines/{id}"))?,
                paged(&format!("pipelines/{id}/jobs"))?.query("include_retried", "true"),
                paged(&format!("pipelines/{id}/bridges"))?,
            ],
            Read::Job(id) => vec![rest(&format!("jobs/{id}"))?],
            Read::JobLog(id) => vec![rest(&format!("jobs/{id}/trace"))?.accept(Accept::Text)],
            Read::PullRequestTemplate => Vec::new(),
            Read::Artifacts(run) => vec![paged(&format!("pipelines/{run}/jobs"))?],
            Read::ArtifactFiles(job) => {
                vec![super::download(rest(&format!("jobs/{job}/artifacts"))?)]
            }
            Read::ArtifactFile { artifact, path } => vec![
                rest(&format!(
                    "jobs/{artifact}/artifacts/{}",
                    path.split('/').map(path_segment).collect::<Vec<_>>().join("/")
                ))?
                .accept(Accept::Raw),
            ],
            Read::PullRequests(numbers) => numbers
                .iter()
                .map(|number| rest(&format!("merge_requests/{number}")))
                .collect::<Result<_, _>>()?,
            Read::Fingerprints(numbers) => numbers
                .iter()
                .map(|number| rest(&format!("merge_requests/{number}")))
                .collect::<Result<_, _>>()?,
            Read::Notifications { filter, page } => {
                let mut request = ApiRequest::get("todos")
                    .query("per_page", page.per_page.clamp(1, PAGE))
                    .query("page", page.number.max(1));
                if !filter.all {
                    request = request.query("state", "pending");
                }
                vec![request]
            }
        })
    }

    fn fetch(
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        if ctx.anonymous()
            && let Some(payload) = anonymous::fetch(ctx, read, cancel)
        {
            return payload.map(Fetched::payload);
        }
        let mut requests = Self::requests(ctx, read)?;
        let class = read.class();
        for request in &mut requests {
            request.class = class;
        }
        let payload = match read {
            Read::Repository | Read::RepositoryNamed(_) => {
                let mut request = requests.remove(0);
                if let Some(etag) = &cond.etag {
                    request.headers.push(("If-None-Match".into(), etag.clone()));
                }
                let response = ctx.send(&request, cancel)?;
                if response.status == 304 {
                    return Ok(Fetched::NotModified);
                }
                if !response.success() {
                    return Err(crate::forge::http::classify(&ctx.endpoints, &response));
                }
                return Ok(Fetched::Payload {
                    value: Payload::Repo(repo_payload(&response.json()?)),
                    etag: response.header("etag").map(str::to_owned),
                    fingerprint: None,
                });
            }
            Read::Viewer => {
                let viewer = user(&ctx.json(&requests[0], cancel)?);
                lock(viewers()).insert(ctx.host().to_string(), viewer.login.clone());
                Payload::User(viewer)
            }
            Read::Labels => Payload::Labels(labels(&Value::Array(
                Self::rest_pages(ctx, requests.remove(0), cancel)?.0,
            ))),
            Read::Milestones => Payload::Milestones(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .0
                    .iter()
                    .filter_map(milestone)
                    .collect(),
            ),
            Read::Collaborators => Payload::Users(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .0
                    .iter()
                    .map(user)
                    .collect(),
            ),
            Read::List { kind, filter, page } => {
                let mut rows = Self::rows(ctx, *kind, filter, *page, cancel)?;
                if let Some(order) = Query::parse(&filter.query).client_sort {
                    super::common::sort_rows(&mut rows.items, &order);
                }
                Payload::Rows(rows)
            }
            Read::Search { kind, filter, page } => {
                let me = if Query::parse(&filter.query).mentions_me() {
                    Some(Self::me(ctx, cancel)?)
                } else {
                    None
                };
                let mut request = search_request(*kind, filter, *page, me.as_deref());
                request.class = class;
                let response = ctx.ok(&request, cancel)?;
                Payload::Rows(PageOf {
                    next: next_page(&response).map(|_| Page {
                        number: page.number + 1,
                        per_page: page.per_page,
                    }),
                    total: total(&response),
                    items: response
                        .json()?
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|value| rest_row(value, *kind))
                        .collect(),
                    other_total: None,
                })
            }
            Read::PullRequest(number) => {
                remember(ctx, *number, ItemKind::PullRequest);
                let pull = Self::merge_request(ctx, *number, requests, cancel)?;
                let fingerprint = Some(pull.fingerprint.clone());
                return Ok(Fetched::Payload {
                    value: Payload::PullRequest(Box::new(pull)),
                    etag: None,
                    fingerprint,
                });
            }
            Read::Fingerprint(number) => {
                remember(ctx, *number, ItemKind::PullRequest);
                let fingerprint = fingerprint(&ctx.json(&requests[0], cancel)?);
                if cond.fingerprint.as_ref() == Some(&fingerprint) {
                    return Ok(Fetched::NotModified);
                }
                Payload::Fingerprint(fingerprint)
            }
            Read::Issue(number) => {
                remember(ctx, *number, ItemKind::Issue);
                // The issue, what closes it and its reactions: one round trip for all three.
                let (value, closed_by, awards) = std::thread::scope(|scope| {
                    let closed_by = scope.spawn(|| Self::optional_list(ctx, &requests[1], cancel));
                    let awards = Self::optional_list(ctx, &requests[2], cancel);
                    let value = ctx.json(&requests[0], cancel);
                    (value, parallel::joined(closed_by), awards)
                });
                let mut issue = issue(&value?, &closed_by?);
                issue.reactions = reactions::gitlab(&Value::Array(awards?));
                Payload::Issue(Box::new(issue))
            }
            Read::Timeline(_) => {
                let mut events = Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .0
                    .iter()
                    .flat_map(timeline_events)
                    .collect::<Vec<_>>();
                // Label and milestone history (GitLab ≥ 11.2 / 13.1); older servers 404.
                for request in requests {
                    events.extend(
                        Self::optional_pages(ctx, request, cancel)?
                            .iter()
                            .filter_map(resource_event),
                    );
                }
                // Notes come oldest first; the resource events slot in by time (stable).
                events.sort_by_key(|event| event_time(event).unwrap_or(i64::MAX));
                Payload::Timeline(events)
            }
            Read::Threads(number) => {
                remember(ctx, *number, ItemKind::PullRequest);
                Payload::Threads(
                    Self::rest_pages(ctx, requests.remove(0), cancel)?
                        .0
                        .iter()
                        .filter_map(|discussion| thread(*number, discussion))
                        .collect(),
                )
            }
            Read::Diff(number) => {
                let files = match Self::rest_pages(ctx, requests.remove(0), cancel) {
                    Ok((files, _)) => files,
                    // `/diffs` is GitLab ≥ 15.7; `/changes` carries the same entries.
                    Err(ForgeError::NotFound(_)) => {
                        let changes = ctx.json(
                            &ApiRequest::get(Self::project(
                                ctx,
                                &format!("merge_requests/{number}/changes"),
                            )?)
                            .class(class),
                            cancel,
                        )?;
                        changes["changes"].as_array().cloned().unwrap_or_default()
                    }
                    Err(error) => return Err(error),
                };
                Payload::Text(assemble_diff(&files))
            }
            Read::CommitDiff(_) => Payload::Text(assemble_diff(
                &Self::rest_pages(ctx, requests.remove(0), cancel)?.0,
            )),
            Read::Viewed(number) => Payload::Viewed(read_viewed(ctx, *number)),
            Read::PullRequestForBranch(_) => Payload::MaybePullRequest(
                ctx.json(&requests[0], cancel)?
                    .as_array()
                    .and_then(|pulls| pulls.first())
                    .map(|pull| count(pull, "iid"))
                    .filter(|number| *number > 0),
            ),
            // One `.gitlab-ci.yml` per project: the rail lists what starts its pipelines.
            Read::Workflows => {
                let mut sources: Vec<String> = Vec::new();
                for pipeline in ctx
                    .json(&requests[0], cancel)?
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(source) = optional(pipeline, "source")
                        && !sources.contains(&source)
                    {
                        sources.push(source);
                    }
                }
                let mut workflows = sources
                    .iter()
                    .map(|source| source_workflow(source))
                    .collect::<Vec<_>>();
                workflows.extend(
                    Self::optional_list(ctx, &requests[1], cancel)?
                        .iter()
                        .map(schedule_workflow),
                );
                Payload::Workflows(workflows)
            }
            Read::WorkflowSource { .. } => Payload::Text(ctx.ok(&requests[0], cancel)?.text()),
            Read::Runs { page, .. } => {
                let mut request = requests.remove(0);
                // §7.5 incremental refresh: only pipelines updated since the cached copy.
                if let Some(since) = &cond.since {
                    request = request.query("updated_after", since.as_str());
                }
                let response = ctx.ok(&request, cancel)?;
                let mut items = response
                    .json()?
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(pipeline_run)
                    .collect::<Vec<_>>();
                Self::source_branches(ctx, &mut items, cancel)?;
                Payload::Runs(PageOf {
                    next: next_page(&response).map(|_| Page {
                        number: page.number + 1,
                        per_page: page.per_page,
                    }),
                    total: total(&response),
                    items,
                    other_total: None,
                })
            }
            Read::Run { .. } => {
                let mut run = vec![pipeline_run(&ctx.json(&requests[0], cancel)?)];
                Self::source_branches(ctx, &mut run, cancel)?;
                let run = run.remove(0);
                let (jobs, _) = Self::rest_pages(ctx, requests[1].clone(), cancel)?;
                let mut jobs = latest_jobs(jobs);
                jobs.extend(Self::optional_list(ctx, &requests[2], cancel)?);
                let jobs = jobs.iter().map(pipeline_job).collect::<Vec<_>>();
                let failure_summary = jobs
                    .iter()
                    .find(|job| job.conclusion == Some(CiState::Failed))
                    .map(|job| FailureLine {
                        job: job.id.clone(),
                        line: format!("{} failed", job.name),
                    });
                Payload::Run(Box::new(RunDetail {
                    run,
                    jobs,
                    failure_summary,
                }))
            }
            Read::Job(_) => Payload::Job(Box::new(pipeline_job(&ctx.json(&requests[0], cancel)?))),
            Read::JobLog(id) => {
                let request = requests.remove(0);
                let cap = request.body_cap;
                let (text, truncated) = match ctx.send(&request, cancel) {
                    Ok(response) if response.success() => (response.text(), false),
                    Ok(response) => {
                        return Err(crate::forge::http::classify(&ctx.endpoints, &response));
                    }
                    Err(ForgeError::BadResponse(message)) if message.contains("exceeds") => (
                        format!(
                            "log exceeds {} MiB · open it in the browser",
                            cap / (1024 * 1024)
                        ),
                        true,
                    ),
                    Err(error) => return Err(error),
                };
                Payload::Log(Log::new(
                    id.clone(),
                    text,
                    LogFormat::GitLabSections,
                    truncated,
                ))
            }
            Read::Artifacts(_) => Payload::Artifacts(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .0
                    .iter()
                    .filter_map(job_artifact)
                    .collect(),
            ),
            Read::ArtifactFiles(_) => {
                let response = ctx.ok(&requests[0], cancel)?;
                super::artifact_payload(read, &response.body)?
            }
            Read::ArtifactFile { .. } => {
                let response = ctx.ok(&requests[0], cancel)?;
                Payload::Text(response.text())
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
                let mut prints = Vec::new();
                for number in numbers {
                    match Self::fetch(
                        ctx,
                        &Read::Fingerprint(*number),
                        &Conditional::default(),
                        cancel,
                    ) {
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
            Read::Notifications { page, .. } => {
                let response = ctx.ok(&requests[0], cancel)?;
                Payload::Notifications(PageOf {
                    next: next_page(&response).map(|_| Page {
                        number: page.number + 1,
                        per_page: page.per_page,
                    }),
                    total: total(&response),
                    items: response
                        .json()?
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(todo)
                        .collect(),
                    other_total: None,
                })
            }
            // §3.7: read from the base ref locally, never through the API.
            Read::PullRequestTemplate => Payload::Template(crate::forge::template::for_context(
                &ctx.root,
                ctx.endpoints.kind,
            )),
        };
        Ok(Fetched::payload(payload))
    }

    /// A secondary list (closing issues, bridges) that older or restricted servers may refuse.
    /// Merge request pipelines run on `refs/merge-requests/N/head`; show the merge request's
    /// source branch instead (one `merge_requests/N` per merge request, remembered).
    fn source_branches(
        ctx: &ForgeContext,
        runs: &mut [Run],
        cancel: &AtomicBool,
    ) -> Result<(), ForgeError> {
        let project = Self::project(ctx, "")?;
        for run in runs.iter_mut() {
            let Some(number) = run
                .branch
                .strip_prefix("refs/merge-requests/")
                .and_then(|rest| rest.split('/').next())
                .and_then(|number| number.parse::<u64>().ok())
            else {
                continue;
            };
            let key = format!("{}|{project}|{number}", ctx.endpoints.api_base);
            let known = lock(mr_branches()).get(&key).cloned();
            let branch = match known {
                Some(branch) => branch,
                None => {
                    let request =
                        ApiRequest::get(Self::project(ctx, &format!("merge_requests/{number}"))?);
                    match ctx.json(&request, cancel) {
                        Ok(value) => match optional(&value, "source_branch") {
                            Some(branch) => {
                                lock(mr_branches()).insert(key, branch.clone());
                                branch
                            }
                            None => continue,
                        },
                        Err(
                            error @ (ForgeError::Cancelled
                            | ForgeError::Timeout
                            | ForgeError::RateLimited { .. }),
                        ) => return Err(error),
                        Err(_) => continue,
                    }
                }
            };
            run.branch = branch;
        }
        Ok(())
    }

    /// Every page of a secondary list, or none when the server lacks it (see `optional_list`).
    fn optional_pages(
        ctx: &ForgeContext,
        request: ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, ForgeError> {
        match Self::rest_pages(ctx, request, cancel) {
            Ok((items, _)) => Ok(items),
            Err(ForgeError::NotAuthenticated { .. }) if ctx.anonymous() => Ok(Vec::new()),
            Err(
                error @ (ForgeError::Cancelled
                | ForgeError::Timeout
                | ForgeError::RateLimited { .. }
                | ForgeError::NotAuthenticated { .. }),
            ) => Err(error),
            Err(_) => Ok(Vec::new()),
        }
    }

    fn optional_list(
        ctx: &ForgeContext,
        request: &ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, ForgeError> {
        match ctx.json(request, cancel) {
            Ok(Value::Array(items)) => Ok(items),
            Ok(_) => Ok(Vec::new()),
            // Anonymous reads of public projects meet a 401 on some secondary lists.
            Err(ForgeError::NotAuthenticated { .. }) if ctx.anonymous() => Ok(Vec::new()),
            Err(
                error @ (ForgeError::Cancelled
                | ForgeError::Timeout
                | ForgeError::RateLimited { .. }
                | ForgeError::NotAuthenticated { .. }),
            ) => Err(error),
            Err(_) => Ok(Vec::new()),
        }
    }

    fn merge_request(
        ctx: &ForgeContext,
        number: u64,
        requests: Vec<ApiRequest>,
        cancel: &AtomicBool,
    ) -> Result<PullRequest, ForgeError> {
        let [detail, approvals, reviewers, commits, diffs, closes, awards] =
            <[ApiRequest; 7]>::try_from(requests)
                .map_err(|_| ForgeError::Failed("merge request plan".into()))?;
        // The parts are independent of each other: one round trip for all of them.
        let (value, approvals, reviewers, commits, diffs, closes, awards) = std::thread::scope(|scope| {
            let approvals = scope.spawn(|| ctx.json(&approvals, cancel));
            let reviewers = scope.spawn(|| Self::optional_list(ctx, &reviewers, cancel));
            let commits = scope.spawn(|| Self::rest_pages(ctx, commits, cancel));
            let diffs = scope.spawn(|| Self::rest_pages(ctx, diffs, cancel));
            let awards = scope.spawn(|| Self::optional_list(ctx, &awards, cancel));
            let closes = Self::optional_list(ctx, &closes, cancel);
            let value = ctx.json(&detail, cancel);
            use super::parallel::joined;
            (value, joined(approvals), joined(reviewers), joined(commits), joined(diffs), closes, joined(awards))
        });
        let value = value?;
        let approvals = match approvals {
            Ok(value) => value,
            Err(ForgeError::NotFound(_) | ForgeError::Forbidden(_)) => Value::Null,
            Err(error) => return Err(error),
        };
        let mut parts = DetailParts {
            approvals,
            reviewers: reviewers?,
            commits: commits?.0,
            diffs: match diffs {
                Ok((diffs, _)) => diffs,
                Err(ForgeError::NotFound(_)) => Vec::new(),
                Err(error) => return Err(error),
            },
            closes: closes?,
            jobs: Vec::new(),
        };
        // A fork's pipeline runs in the source project, so its jobs are read there.
        let pipeline = &value["head_pipeline"];
        if let (Some(id), Some(project)) =
            (pipeline["id"].as_u64(), pipeline["project_id"].as_u64())
        {
            let request = ApiRequest::get(format!("projects/{project}/pipelines/{id}/jobs"))
                .query("per_page", PAGE)
                .class(detail.class);
            parts.jobs = match Self::rest_pages(ctx, request, cancel) {
                Ok((jobs, _)) => jobs,
                Err(ForgeError::NotFound(_) | ForgeError::Forbidden(_)) => Vec::new(),
                Err(error) => return Err(error),
            };
        }
        if count(&value, "iid") != number {
            return Err(ForgeError::NotFound(format!("merge request !{number}")));
        }
        let mut pull = pull_request(&value, parts);
        pull.reactions = reactions::gitlab(&Value::Array(awards?));
        Ok(pull)
    }

    // -------------------------------------------------------------------------- writes

    fn user_id(ctx: &ForgeContext, login: &str, cancel: &AtomicBool) -> Result<u64, ForgeError> {
        let login = if login.trim().eq_ignore_ascii_case("@me") {
            Self::me(ctx, cancel)?
        } else {
            login.trim().trim_start_matches('@').to_owned()
        };
        ctx.json(&ApiRequest::get("users").query("username", &login), cancel)?
            .as_array()
            .and_then(|users| users.first())
            .and_then(|user| user["id"].as_u64())
            .ok_or_else(|| ForgeError::NotFound(format!("user {login}")))
    }

    fn milestone_id(
        ctx: &ForgeContext,
        title: &str,
        cancel: &AtomicBool,
    ) -> Result<u64, ForgeError> {
        let request = ApiRequest::get(Self::project(ctx, "milestones")?)
            .query("title", title.trim())
            .query("include_ancestors", "true");
        ctx.json(&request, cancel)?
            .as_array()
            .into_iter()
            .flatten()
            .find(|milestone| text(milestone, "title").eq_ignore_ascii_case(title.trim()))
            .and_then(|milestone| milestone["id"].as_u64())
            .ok_or_else(|| ForgeError::NotFound(format!("milestone {title}")))
    }

    /// The ids a people field ends with: current minus `remove`, plus `add`.
    fn people(
        ctx: &ForgeContext,
        current: &Value,
        add: &[String],
        remove: &[String],
        cancel: &AtomicBool,
    ) -> Result<Vec<u64>, ForgeError> {
        let mut ids = Vec::new();
        for person in current.as_array().into_iter().flatten() {
            let login = text(person, "username");
            if remove
                .iter()
                .any(|gone| gone.trim_start_matches('@').eq_ignore_ascii_case(&login))
            {
                continue;
            }
            if let Some(id) = person["id"].as_u64() {
                ids.push(id);
            }
        }
        for login in add {
            let id = Self::user_id(ctx, login, cancel)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn diff_refs(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<Value, ForgeError> {
        ctx.json(
            &ApiRequest::get(Self::project(ctx, &format!("merge_requests/{number}"))?),
            cancel,
        )
    }

    fn position(refs: &Value, path: &str, line: u64, side: Side) -> Value {
        let mut position = json!({
            "position_type": "text",
            "base_sha": text(refs, "base_sha"),
            "start_sha": text(refs, "start_sha"),
            "head_sha": text(refs, "head_sha"),
            "old_path": path,
            "new_path": path,
        });
        match side {
            Side::Right => position["new_line"] = line.into(),
            Side::Left => position["old_line"] = line.into(),
        }
        position
    }

    /// Request changes is GraphQL-only and newer than approvals; ask the schema once per host.
    fn can_request_changes(ctx: &ForgeContext, cancel: &AtomicBool) -> Result<bool, ForgeError> {
        if let Some(known) = observed(ctx, REQUEST_CHANGES) {
            return Ok(known);
        }
        let data = Self::graphql(ctx, MUTATIONS_DOCUMENT, json!({}), cancel)?;
        let supported = nodes(&data["__type"]["fields"])
            .iter()
            .any(|field| field["name"] == "mergeRequestRequestChanges");
        record(ctx, REQUEST_CHANGES, supported);
        Ok(supported)
    }

    fn submit_review(
        ctx: &ForgeContext,
        number: u64,
        verdict: Verdict,
        body: &str,
        drafts: &[InlineDraft],
        head: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<&'static str, ForgeError> {
        // Refuse before sending anything the forge cannot finish.
        if verdict == Verdict::RequestChanges && !Self::can_request_changes(ctx, cancel)? {
            return Err(ForgeError::Unsupported {
                capability: REQUEST_CHANGES,
                reason: "this GitLab has no request-changes · comment, or approve later".into(),
            });
        }
        let mr = format!("merge_requests/{number}");
        let post = |tail: &str, payload: Value| -> Result<Value, ForgeError> {
            ctx.json(
                &ApiRequest::new(Method::Post, Self::project(ctx, tail)?).json(payload),
                cancel,
            )
        };
        let mut batched = false;
        if !drafts.is_empty() {
            let refs = Self::diff_refs(ctx, number, cancel)?;
            if let Some(head) = head.filter(|head| !head.is_empty())
                && text(&refs, "sha") != head
            {
                return Err(ForgeError::Conflict(MOVED.into()));
            }
            let refs = &refs["diff_refs"];
            batched = observed(ctx, DRAFT_NOTES) != Some(false);
            for draft in drafts {
                let position = Self::position(refs, &draft.path, draft.line, draft.side);
                if batched {
                    match post(
                        &format!("{mr}/draft_notes"),
                        json!({ "note": draft.range_body(), "position": position }),
                    ) {
                        Ok(_) => continue,
                        // Draft notes are GitLab ≥ 15.9: post the rest one by one.
                        Err(ForgeError::NotFound(_)) => {
                            record(ctx, DRAFT_NOTES, false);
                            batched = false;
                        }
                        Err(error) => return Err(error),
                    }
                }
                post(
                    &format!("{mr}/discussions"),
                    json!({ "body": draft.range_body(), "position": position }),
                )?;
            }
        }
        if !body.trim().is_empty() {
            if batched {
                post(&format!("{mr}/draft_notes"), json!({ "note": body }))?;
            } else {
                post(&format!("{mr}/notes"), json!({ "body": body }))?;
            }
        }
        if batched {
            post(&format!("{mr}/draft_notes/bulk_publish"), json!({}))?;
        }
        Ok(match verdict {
            Verdict::Approve => {
                let payload = match head.filter(|head| !head.is_empty()) {
                    Some(head) => json!({ "sha": head }),
                    None => json!({}),
                };
                post(&format!("{mr}/approve"), payload).map_err(|error| conflict(error, MOVED))?;
                "Approved"
            }
            Verdict::RequestChanges => {
                let data = Self::graphql(
                    ctx,
                    REQUEST_CHANGES_DOCUMENT,
                    json!({ "projectPath": ctx.repo()?.full(), "iid": number.to_string() }),
                    cancel,
                )?;
                let errors = nodes(&data["mergeRequestRequestChanges"]["errors"])
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                if !errors.is_empty() {
                    return Err(ForgeError::Failed(errors.join(" · ")));
                }
                "Changes requested"
            }
            Verdict::Comment => "Review saved",
        })
    }

    /// `POST …/unapprove`: there is no `Write` for it yet, so the UI cannot reach it (F7).
    pub fn unapprove(
        ctx: &ForgeContext,
        number: u64,
        cancel: &AtomicBool,
    ) -> Result<(), ForgeError> {
        ctx.json(
            &ApiRequest::new(
                Method::Post,
                Self::project(ctx, &format!("merge_requests/{number}/unapprove"))?,
            ),
            cancel,
        )
        .map(|_| ())
    }

    fn merge(
        ctx: &ForgeContext,
        number: u64,
        method: MergeMethod,
        head: &str,
        delete_branch: bool,
        commit_message: Option<String>,
        cancel: &AtomicBool,
    ) -> Result<&'static str, ForgeError> {
        let mr = format!("merge_requests/{number}");
        let mut current = Self::diff_refs(ctx, number, cancel)?;
        if !head.is_empty() && text(&current, "sha") != head {
            return Err(ForgeError::Conflict(MOVED.into()));
        }
        if method == MergeMethod::Rebase {
            ctx.ok(
                &ApiRequest::new(Method::Put, Self::project(ctx, &format!("{mr}/rebase"))?),
                cancel,
            )
            .map_err(|error| conflict(error, MOVED))?;
            let started = std::time::Instant::now();
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(ForgeError::Cancelled);
                }
                current = ctx.json(
                    &ApiRequest::get(Self::project(ctx, &mr)?)
                        .query("include_rebase_in_progress", "true"),
                    cancel,
                )?;
                if current["rebase_in_progress"].as_bool() != Some(true) {
                    break;
                }
                if started.elapsed() > REBASE_WAIT {
                    return Err(ForgeError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if let Some(error) = optional(&current, "merge_error") {
                return Err(ForgeError::Conflict(error));
            }
        }
        let sha = if method == MergeMethod::Rebase {
            text(&current, "sha")
        } else {
            head.to_owned()
        };
        let pipeline = text(&current["head_pipeline"], "status");
        let waits = pipeline_state(&pipeline) == CiState::Pending;
        let mut payload = json!({
            "should_remove_source_branch": delete_branch,
            "squash": method == MergeMethod::Squash,
        });
        if !sha.is_empty() {
            payload["sha"] = sha.into();
        }
        if waits {
            payload["merge_when_pipeline_succeeds"] = true.into();
        }
        if let Some(commit_message) = commit_message {
            let key = if method == MergeMethod::Squash {
                "squash_commit_message"
            } else {
                "merge_commit_message"
            };
            payload[key] = commit_message.into();
        }
        let merged = ctx
            .json(
                &ApiRequest::new(Method::Put, Self::project(ctx, &format!("{mr}/merge"))?)
                    .json(payload),
                cancel,
            )
            .map_err(|error| conflict(error, MOVED))?;
        Ok(if merged["state"] == "merged" {
            "Merged"
        } else if waits {
            "Merges when the pipeline succeeds"
        } else {
            "Merge requested"
        })
    }

    /// The single-request writes, as `ApiRequest`s (the contract checks their bodies).
    pub fn write_requests(
        ctx: &ForgeContext,
        write: &Write,
    ) -> Result<Vec<ApiRequest>, ForgeError> {
        let put = |tail: &str, payload: Value| -> Result<ApiRequest, ForgeError> {
            Ok(ApiRequest::new(Method::Put, Self::project(ctx, tail)?).json(payload))
        };
        let post = |tail: &str, payload: Value| -> Result<ApiRequest, ForgeError> {
            Ok(ApiRequest::new(Method::Post, Self::project(ctx, tail)?).json(payload))
        };
        Ok(match write {
            Write::Edit {
                kind,
                number,
                title,
                body,
            } => vec![put(
                &item_path(*kind, *number),
                json!({ "title": title, "description": body }),
            )?],
            Write::Comment { kind, number, body } => vec![post(
                &format!("{}/notes", item_path(*kind, *number)),
                json!({ "body": body }),
            )?],
            Write::SetState {
                kind, number, open, ..
            } => vec![put(
                &item_path(*kind, *number),
                json!({ "state_event": if *open { "reopen" } else { "close" } }),
            )?],
            Write::SetLabels {
                kind,
                number,
                add,
                remove,
            } => {
                let mut payload = json!({});
                if !add.is_empty() {
                    payload["add_labels"] = add.join(",").into();
                }
                if !remove.is_empty() {
                    payload["remove_labels"] = remove.join(",").into();
                }
                vec![put(&item_path(*kind, *number), payload)?]
            }
            Write::Reply {
                number,
                thread,
                body,
            } => {
                let discussion = thread.0.rsplit('/').next().unwrap_or_default();
                vec![post(
                    &format!("merge_requests/{number}/discussions/{discussion}/notes"),
                    json!({ "body": body }),
                )?]
            }
            Write::CommitSuggestion { .. } => {
                // `PUT suggestions/:id/apply` needs the note's suggestion id, which the
                // discussions read does not keep (DESIGN.md §3.30a).
                return Err(Self::unsupported(
                    "commit_suggestion",
                    "Apply this suggestion on GitLab · o opens the merge request",
                ));
            }
            Write::Resolve { thread, resolved } => {
                let (number, discussion) = thread.0.split_once('/').ok_or_else(|| {
                    ForgeError::Failed(format!("not a GitLab discussion: {}", thread.0))
                })?;
                vec![put(
                    &format!("merge_requests/{number}/discussions/{discussion}"),
                    json!({ "resolved": resolved }),
                )?]
            }
            Write::Subscribe {
                kind,
                number,
                state,
                ..
            } => vec![post(
                &format!(
                    "{}/{}",
                    item_path(*kind, *number),
                    if *state == SubscriptionState::Subscribed {
                        "subscribe"
                    } else {
                        "unsubscribe"
                    }
                ),
                json!({}),
            )?],
            Write::Rerun {
                run,
                scope: RerunScope::Failed,
                ..
            } => vec![post(&format!("pipelines/{run}/retry"), json!({}))?],
            Write::RerunJob(job) => vec![post(&format!("jobs/{job}/retry"), json!({}))?],
            Write::CancelRun(run) => vec![post(&format!("pipelines/{run}/cancel"), json!({}))?],
            Write::Dispatch {
                reference, inputs, ..
            } => vec![post(
                "pipeline",
                json!({
                    "ref": reference,
                    "variables": inputs.iter().map(|(key, value)| json!({ "key": key, "value": value })).collect::<Vec<_>>(),
                }),
            )?],
            Write::MarkRead(id) => vec![ApiRequest::new(
                Method::Post,
                format!("todos/{id}/mark_as_done"),
            )],
            Write::MarkAllRead => vec![ApiRequest::new(Method::Post, "todos/mark_as_done")],
            _ => Vec::new(),
        })
    }

    fn unsupported(capability: &'static str, reason: &str) -> ForgeError {
        ForgeError::Unsupported {
            capability,
            reason: reason.into(),
        }
    }

    fn web_note(ctx: &ForgeContext, kind: ItemKind, number: u64, note: &Value) -> Option<String> {
        let repo = ctx.repo.as_ref()?;
        let id = note["id"].as_u64()?;
        let section = match kind {
            ItemKind::Issue => "issues",
            ItemKind::PullRequest => "merge_requests",
        };
        Some(format!(
            "{}/{}/-/{section}/{number}#note_{id}",
            ctx.endpoints.web_base,
            repo.full()
        ))
    }

    fn perform(
        ctx: &ForgeContext,
        write: &Write,
        cancel: &AtomicBool,
    ) -> Result<Receipt, ForgeError> {
        let invalidates = write.invalidates();
        let saved = |summary: &str| Receipt {
            summary: summary.into(),
            web_url: None,
            created_number: None,
            invalidates: invalidates.clone(),
        };
        let item = |kind: ItemKind, number: u64| -> Result<Value, ForgeError> {
            remember(ctx, number, kind);
            ctx.json(
                &ApiRequest::get(Self::project(ctx, &item_path(kind, number))?),
                cancel,
            )
        };
        match write {
            Write::CreateIssue {
                title,
                body,
                labels,
                assignees,
            } => {
                let mut payload = json!({ "title": title, "description": body });
                if !labels.is_empty() {
                    payload["labels"] = labels.join(",").into();
                }
                if !assignees.is_empty() {
                    payload["assignee_ids"] =
                        Self::people(ctx, &Value::Null, assignees, &[], cancel)?.into();
                }
                let created = ctx.json(
                    &ApiRequest::new(Method::Post, Self::project(ctx, "issues")?).json(payload),
                    cancel,
                )?;
                remember(ctx, count(&created, "iid"), ItemKind::Issue);
                Ok(Receipt {
                    summary: "Saved".into(),
                    web_url: optional(&created, "web_url"),
                    created_number: created["iid"].as_u64(),
                    invalidates,
                })
            }
            Write::CreatePullRequest(request) => {
                let source = path_segment(&request.head_repository);
                let mut payload = json!({
                    "source_branch": request.head_branch,
                    "target_branch": request.base_branch,
                    "title": if request.draft && !request.title.to_ascii_lowercase().starts_with("draft:") {
                        format!("Draft: {}", request.title)
                    } else {
                        request.title.clone()
                    },
                    "description": request.body,
                });
                if !request
                    .head_repository
                    .eq_ignore_ascii_case(&request.base_repository)
                {
                    let target = ctx.json(
                        &ApiRequest::get(format!("projects/{}", path_segment(&request.base_repository))),
                        cancel,
                    )?;
                    payload["target_project_id"] = target["id"].clone();
                }
                let created = ctx.json(
                    &ApiRequest::new(Method::Post, format!("projects/{source}/merge_requests"))
                        .json(payload)
                        .body_file(),
                    cancel,
                )?;
                Ok(Receipt {
                    summary: "Saved".into(),
                    web_url: optional(&created, "web_url"),
                    created_number: created["iid"].as_u64(),
                    invalidates,
                })
            }
            Write::Comment { kind, number, body } => {
                remember(ctx, *number, *kind);
                let request = Self::write_requests(ctx, write)?.remove(0);
                let note = ctx.json(&request, cancel)?;
                Ok(Receipt {
                    web_url: Self::web_note(ctx, *kind, *number, &note),
                    ..saved(if body.is_empty() {
                        "Saved"
                    } else {
                        "Comment saved"
                    })
                })
            }
            Write::SetState {
                reason: Some(ClosedReason::NotPlanned | ClosedReason::Duplicate),
                open: false,
                ..
            } => Err(Self::unsupported(
                "issue_not_planned",
                "GitLab closes issues without a reason",
            )),
            Write::SetAssignees {
                kind,
                number,
                add,
                remove,
            } => {
                let current = item(*kind, *number)?;
                let ids = Self::people(ctx, &current["assignees"], add, remove, cancel)?;
                ctx.json(
                    &ApiRequest::new(Method::Put, Self::project(ctx, &item_path(*kind, *number))?)
                        .json(json!({ "assignee_ids": ids })),
                    cancel,
                )?;
                Ok(saved("Saved"))
            }
            Write::SetReviewers {
                number,
                add,
                remove,
            } => {
                let current = item(ItemKind::PullRequest, *number)?;
                let ids = Self::people(ctx, &current["reviewers"], add, remove, cancel)?;
                ctx.json(
                    &ApiRequest::new(
                        Method::Put,
                        Self::project(ctx, &format!("merge_requests/{number}"))?,
                    )
                    .json(json!({ "reviewer_ids": ids })),
                    cancel,
                )?;
                Ok(saved("Saved"))
            }
            Write::SetMilestone {
                kind,
                number,
                milestone,
            } => {
                let id = match milestone
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                {
                    Some(title) => Self::milestone_id(ctx, title, cancel)?,
                    None => 0,
                };
                ctx.json(
                    &ApiRequest::new(Method::Put, Self::project(ctx, &item_path(*kind, *number))?)
                        .json(json!({ "milestone_id": id })),
                    cancel,
                )?;
                Ok(saved("Saved"))
            }
            Write::SetProjects { .. } => Err(Self::unsupported(
                "projects",
                "GitLab issue boards are not part of the forge tabs",
            )),
            Write::InlineComment {
                number,
                path,
                line,
                side,
                head,
                body,
            } => {
                let current = Self::diff_refs(ctx, *number, cancel)?;
                if !head.is_empty() && text(&current, "sha") != *head {
                    return Err(ForgeError::Conflict(MOVED.into()));
                }
                let position = Self::position(&current["diff_refs"], path, *line, *side);
                ctx.json(
                    &ApiRequest::new(
                        Method::Post,
                        Self::project(ctx, &format!("merge_requests/{number}/discussions"))?,
                    )
                    .json(json!({ "body": body, "position": position })),
                    cancel,
                )?;
                Ok(saved("Comment saved"))
            }
            Write::Reply { .. } => {
                ctx.json(&Self::write_requests(ctx, write)?.remove(0), cancel)?;
                Ok(saved("Reply saved"))
            }
            Write::Resolve { resolved, .. } => {
                ctx.json(&Self::write_requests(ctx, write)?.remove(0), cancel)?;
                Ok(saved(if *resolved { "Resolved" } else { "Unresolved" }))
            }
            Write::SubmitReview {
                number,
                verdict,
                body,
                drafts,
                head,
            } => {
                let summary = Self::submit_review(
                    ctx,
                    *number,
                    *verdict,
                    body,
                    drafts,
                    head.as_deref(),
                    cancel,
                )?;
                Ok(saved(summary))
            }
            Write::Merge {
                number,
                method,
                head,
                delete_branch,
                title,
                message,
            } => {
                let title = title.as_deref().filter(|title| !title.is_empty());
                let message = message.as_deref().filter(|message| !message.is_empty());
                let commit_message = match (title, message) {
                    (Some(title), Some(message)) => Some(format!("{title}\n\n{message}")),
                    (title, message) => title.or(message).map(str::to_owned),
                };
                let summary = Self::merge(
                    ctx,
                    *number,
                    *method,
                    head,
                    *delete_branch,
                    commit_message,
                    cancel,
                )?;
                Ok(saved(summary))
            }
            Write::SetViewed {
                number,
                path,
                viewed,
                ..
            } => {
                write_viewed(ctx, *number, path, *viewed)?;
                Ok(saved("Saved · local"))
            }
            Write::Subscribe { .. } => {
                // 304 means "already so": not an error.
                let response = ctx.send(&Self::write_requests(ctx, write)?.remove(0), cancel)?;
                if !response.success() && response.status != 304 {
                    return Err(crate::forge::http::classify(&ctx.endpoints, &response));
                }
                Ok(saved("Saved"))
            }
            Write::Rerun {
                run,
                scope: RerunScope::All,
                ..
            } => {
                // GitLab retries failed jobs only; "run again" is a new pipeline on the same ref.
                let pipeline = ctx.json(
                    &ApiRequest::get(Self::project(ctx, &format!("pipelines/{run}"))?),
                    cancel,
                )?;
                let created = ctx.json(
                    &ApiRequest::new(Method::Post, Self::project(ctx, "pipeline")?)
                        .json(json!({ "ref": text(&pipeline, "ref") })),
                    cancel,
                )?;
                Ok(Receipt {
                    web_url: optional(&created, "web_url"),
                    ..saved("New pipeline started")
                })
            }
            Write::ApproveRun { .. } => Err(Self::unsupported(
                "approve_fork_run",
                "GitLab runs fork pipelines from the merge request page",
            )),
            Write::DownloadArtifact {
                artifact,
                name,
                directory,
            } => {
                let request = super::download(ApiRequest::get(Self::project(
                    ctx,
                    &format!("jobs/{artifact}/artifacts"),
                )?));
                let response = ctx.ok(&request, cancel)?;
                super::save_artifact(&response.body, name, directory)
            }
            Write::Unsubscribe(_) => Err(Self::unsupported(
                "subscribe",
                "a To-do has no subscription · mark it done instead",
            )),
            Write::Checkout { .. }
            | Write::Worktree { .. }
            | Write::Develop { .. }
            | Write::OpenInBrowser { .. } => Err(Self::unsupported(
                "porcelain",
                "this action runs locally, not through the API",
            )),
            _ => {
                let requests = Self::write_requests(ctx, write)?;
                if requests.is_empty() {
                    return Err(Self::unsupported("write", "not available on GitLab"));
                }
                let mut last = Value::Null;
                for request in requests {
                    if cancel.load(Ordering::Acquire) {
                        return Err(ForgeError::Cancelled);
                    }
                    last = ctx.json(&request, cancel)?;
                }
                let summary = match write {
                    Write::Rerun { .. } => "Retrying failed jobs",
                    Write::RerunJob(_) => "Retrying job",
                    Write::CancelRun(_) => "Cancelling",
                    Write::Dispatch { .. } => "Pipeline started",
                    Write::MarkRead(_) | Write::MarkAllRead => "Done",
                    _ => "Saved",
                };
                Ok(Receipt {
                    web_url: optional(&last, "web_url"),
                    ..saved(summary)
                })
            }
        }
    }
}

/// GitLab's matrix (§4.2): request changes and draft notes are probed, viewed files are local;
/// pipelines list, retry, cancel and run with variables, job artifacts are per job, and the
/// workflows rail is the pipeline sources.
pub fn capabilities(graphql_batching: bool) -> Capabilities {
    Capabilities {
        review_request_changes: Cap::Probe,
        batched_review: Cap::Probe,
        merge_methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
        update_branch: Cap::No("GitLab rebases via the merge sheet"),
        issue_not_planned: Cap::No("GitLab closes issues without a reason"),
        projects: Cap::No("GitLab issue boards are not part of the forge tabs"),
        job_steps: StepsKind::FromLogSections,
        dispatch_inputs_schema: Cap::No("pipeline variables are free-form"),
        approve_fork_run: Cap::No("GitLab runs fork pipelines from the merge request page"),
        attempts: Cap::No("GitLab retries jobs, not pipelines"),
        run_filters: RunFilters {
            workflow: true,
            branch: true,
            event: true,
            status: true,
        },
        graphql_batching: if graphql_batching {
            Cap::Yes
        } else {
            Cap::No("no GraphQL endpoint")
        },
        ..Capabilities::github(true)
    }
}

impl Provider for GitLab {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitLab
    }

    fn terms(&self) -> &'static Terms {
        &GITLAB_TERMS
    }

    fn capabilities(&self, ctx: &ForgeContext) -> Capabilities {
        let mut capabilities = capabilities(ctx.transport().graphql_batching());
        match observed(ctx, REQUEST_CHANGES) {
            Some(true) => capabilities.review_request_changes = Cap::Yes,
            Some(false) => {
                capabilities.review_request_changes =
                    Cap::No("this GitLab has no request-changes · comment, or approve later")
            }
            None => {}
        }
        if observed(ctx, DRAFT_NOTES) == Some(false) {
            capabilities.batched_review = Cap::No("draft notes need GitLab 15.9");
        }
        if ctx.anonymous() {
            return capabilities.read_only(ProviderKind::GitLab);
        }
        capabilities
    }

    fn plan(&self, ctx: &ForgeContext, read: &Read, _cond: &Conditional) -> Plan {
        let note = match read {
            Read::Viewed(_) => Some("viewed files are kept locally"),
            Read::Workflows => Some("pipeline sources of one .gitlab-ci.yml"),
            Read::PullRequestTemplate => Some("read locally from the base branch"),
            _ => None,
        };
        match Self::requests(ctx, read) {
            Ok(requests) => Plan { requests, note },
            Err(_) => Plan::default(),
        }
    }

    fn read(
        &self,
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        ctx.start_deadline(super::read_budget(read));
        let result = Self::fetch(ctx, read, cond, cancel);
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
            crate::forge::worker::local_write(ProviderKind::GitLab, write),
        )?;
        Self::perform(ctx, write, cancel)
    }

    fn incremental(&self, _ctx: &ForgeContext, read: &Read) -> bool {
        // A status filter is left out: a pipeline that left the status would linger.
        matches!(read, Read::Runs { filter, page } if page.number <= 1 && filter.status.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::credential::Env;

    #[test]
    fn an_items_kind_outlives_a_restart_in_the_disk_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = crate::forge::cache::Cache::at(root.path());
        let key = ("gitlab.example".to_string(), "group/kind-test".to_string(), 41);
        let recalled = |key: &(String, String, u64)| lock(kinds()).get(key).copied();
        lock(kinds()).insert(key.clone(), ItemKind::Issue);
        keep_in(&cache, key.clone());
        // A restart: the process forgets, the disk does not.
        lock(kinds()).remove(&key);
        restore_in(&cache, key.clone());
        assert_eq!(recalled(&key), Some(ItemKind::Issue));
        // Read as a merge request since: that is what a later timeline follows.
        lock(kinds()).insert(key.clone(), ItemKind::PullRequest);
        keep_in(&cache, key.clone());
        lock(kinds()).remove(&key);
        restore_in(&cache, key.clone());
        assert_eq!(recalled(&key), Some(ItemKind::PullRequest));
        // What this process knows wins over the disk.
        lock(kinds()).insert(key.clone(), ItemKind::Issue);
        restore_in(&cache, key.clone());
        assert_eq!(recalled(&key), Some(ItemKind::Issue));
        // Nothing known anywhere: nothing restored (the read falls back as before).
        let unknown = ("gitlab.example".to_string(), "group/kind-test".to_string(), 42);
        restore_in(&cache, unknown.clone());
        assert_eq!(recalled(&unknown), None);
    }
    use crate::forge::transport::HostKey;

    fn ctx() -> ForgeContext {
        ForgeContext::offline(
            crate::forge::detect::endpoints_for(
                ProviderKind::GitLab,
                &HostKey::new("gitlab.com"),
                None,
                &Env::from_pairs::<&str, &str>([]),
            ),
            super::super::RepoRef::parse("orbit/team/orbit"),
        )
    }

    #[test]
    fn nested_groups_become_one_encoded_project_id() {
        let ctx = ctx();
        let plan = GitLab.plan(&ctx, &Read::PullRequest(564), &Conditional::default());
        assert_eq!(
            plan.requests[0].path_and_query(),
            "projects/orbit%2Fteam%2Forbit/merge_requests/564"
        );
        assert_eq!(ctx.endpoints.api_base, "https://gitlab.com/api/v4");
        assert_eq!(
            GitLab::write_requests(
                &ctx,
                &Write::Resolve {
                    thread: ItemId::from("564/6a9c"),
                    resolved: true,
                },
            )
            .unwrap()[0]
                .path_and_query(),
            "projects/orbit%2Fteam%2Forbit/merge_requests/564/discussions/6a9c"
        );
    }

    #[test]
    fn every_read_has_a_plan_or_a_note() {
        let ctx = ctx();
        for read in super::super::sample_reads() {
            let plan = GitLab.plan(&ctx, &read, &Conditional::default());
            assert!(
                !plan.requests.is_empty() || plan.note.is_some(),
                "{read:?} has no plan"
            );
        }
    }

    #[test]
    fn github_search_terms_translate_to_gitlab_arguments() {
        let filter = Filter {
            state: StateFilter::Merged,
            query: "author:@me review-requested:sam label:\"needs review\" -label:wip is:draft milestone:v1.4 flaky test".into(),
        };
        let (document, variables) = list_document(
            ItemKind::PullRequest,
            "orbit/team/orbit",
            &filter,
            Some("maya"),
        );
        assert!(document.starts_with("query MergeRequestList("));
        assert!(document.contains("mergeRequests(first: $first, after: $after, state: $state"));
        assert_eq!(variables["path"], "orbit/team/orbit");
        assert_eq!(variables["state"], "merged");
        assert_eq!(variables["sort"], "UPDATED_DESC");
        assert_eq!(variables["authorUsername"], "maya");
        assert_eq!(variables["reviewerUsername"], "sam");
        assert_eq!(variables["labels"], json!(["needs review"]));
        assert_eq!(variables["not"], json!({ "labels": ["wip"] }));
        assert_eq!(variables["draft"], true);
        assert_eq!(variables["milestoneTitle"], "v1.4");
        assert_eq!(variables["search"], "flaky test");

        let (document, variables) = list_document(
            ItemKind::Issue,
            "orbit/team/orbit",
            &Filter::default(),
            None,
        );
        assert!(document.starts_with("query IssueList("));
        assert!(!document.contains("search"));
        assert_eq!(variables["state"], "opened");

        let request = search_request(
            ItemKind::PullRequest,
            &Filter {
                state: StateFilter::Open,
                query: "review-requested:@me".into(),
            },
            Page::first(50),
            Some("maya"),
        );
        assert_eq!(
            request.path_and_query(),
            "merge_requests?scope=all&state=opened&reviewer_username=maya&order_by=updated_at&sort=desc&per_page=50&page=1"
        );
    }

    #[test]
    fn diffs_regain_git_headers() {
        let files = json!([
            {"old_path": "a.rs", "new_path": "a.rs", "a_mode": "100644", "b_mode": "100644",
             "new_file": false, "deleted_file": false, "renamed_file": false, "diff": "@@ -1 +1 @@\n-a\n+b\n"},
            {"old_path": "new.rs", "new_path": "new.rs", "a_mode": "0", "b_mode": "100644",
             "new_file": true, "deleted_file": false, "renamed_file": false, "diff": "@@ -0,0 +1 @@\n+x"},
            {"old_path": "old.rs", "new_path": "moved.rs", "a_mode": "100644", "b_mode": "100644",
             "new_file": false, "deleted_file": false, "renamed_file": true, "diff": ""}
        ]);
        let diff = assemble_diff(files.as_array().unwrap());
        assert_eq!(
            diff,
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+x\n\
diff --git a/old.rs b/moved.rs\nrename from old.rs\nrename to moved.rs\n"
        );
        let file = changed_file(&files[0]);
        assert_eq!((file.additions, file.deletions), (1, 1));
    }

    #[test]
    fn diff_discussions_become_threads_and_individual_notes_do_not() {
        let discussions = json!([
            {"id": "d1", "individual_note": false, "notes": [
                {"id": 1, "type": "DiffNote", "body": "nit", "author": {"username": "alex"}, "system": false,
                 "resolvable": true, "resolved": true,
                 "position": {"new_path": "src/lib.rs", "old_path": "src/lib.rs", "new_line": null, "old_line": 7, "head_sha": "c92"}},
                {"id": 2, "type": "DiffNote", "body": "done", "author": {"username": "maya"}, "system": false,
                 "resolvable": true, "resolved": true, "position": {"new_path": "src/lib.rs", "old_line": 7}}
            ]},
            {"id": "d2", "individual_note": true, "notes": [{"id": 3, "body": "LGTM", "author": {"username": "sam"}}]}
        ]);
        let threads = discussions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|discussion| thread(13, discussion))
            .collect::<Vec<_>>();
        assert_eq!(threads.len(), 1);
        let thread = &threads[0];
        assert_eq!(thread.id.0, "13/d1");
        assert_eq!(thread.resolved, Some(true));
        assert_eq!(
            (thread.path.as_deref(), thread.line, thread.side),
            (Some("src/lib.rs"), Some(7), Side::Left)
        );
        assert_eq!(thread.comments.len(), 2);
    }

    #[test]
    fn rows_carry_review_pipeline_and_size_slots() {
        let node = json!({
            "iid": "564", "id": "gid://gitlab/MergeRequest/1", "title": "Update tools", "draft": true,
            "state": "opened", "webUrl": "https://gitlab.com/orbit/team/orbit/-/merge_requests/564",
            "updatedAt": "2026-11-12T09:39:26Z", "userNotesCount": 16, "author": {"username": "maya"},
            "labels": {"nodes": [{"title": "backend", "color": "#D10069"}]}, "approved": true,
            "reviewers": {"nodes": [{"username": "sam"}]}, "headPipeline": {"status": "FAILED"},
            "diffStatsSummary": {"additions": 15, "deletions": 12, "fileCount": 1}
        });
        let row = graphql_row(&node, ItemKind::PullRequest, "orbit/team/orbit");
        assert_eq!(row.number, 564);
        assert_eq!(row.lifecycle, Lifecycle::Draft);
        assert_eq!(row.review, Some(ReviewState::Approved));
        assert_eq!(row.ci, Some(CiState::Failed));
        assert_eq!(row.ci_counts, [(CiState::Failed, 1)]);
        assert_eq!(
            (row.additions, row.deletions, row.changed_files),
            (Some(15), Some(12), Some(1))
        );
        assert_eq!(row.labels[0].name, "backend");
    }

    #[test]
    fn graphql_errors_without_data_classify() {
        let missing = json!({"data": {"project": null}, "errors": [{"message": "The resource that you are attempting to access does not exist or you don't have permission to perform this action"}]});
        assert!(matches!(
            graphql_data(missing),
            Err(ForgeError::NotFound(_))
        ));
        assert!(graphql_data(json!({"data": {"project": {"id": 1}}})).is_ok());
    }

    #[test]
    fn writes_send_json_bodies() {
        let ctx = ctx();
        let labels = GitLab::write_requests(
            &ctx,
            &Write::SetLabels {
                kind: ItemKind::PullRequest,
                number: 13,
                add: vec!["bug".into(), "ui".into()],
                remove: vec!["wip".into()],
            },
        )
        .unwrap();
        assert_eq!(labels[0].method, Method::Put);
        assert_eq!(
            labels[0].path_and_query(),
            "projects/orbit%2Fteam%2Forbit/merge_requests/13"
        );
        assert_eq!(
            labels[0].body,
            Some(crate::forge::transport::Body::Json(
                json!({"add_labels": "bug,ui", "remove_labels": "wip"})
            ))
        );
        let close = GitLab::write_requests(
            &ctx,
            &Write::SetState {
                kind: ItemKind::Issue,
                number: 7,
                open: false,
                reason: None,
            },
        )
        .unwrap();
        assert_eq!(
            close[0].body,
            Some(crate::forge::transport::Body::Json(
                json!({"state_event": "close"})
            ))
        );
        assert_eq!(
            close[0].path_and_query(),
            "projects/orbit%2Fteam%2Forbit/issues/7"
        );
    }

    #[test]
    fn pipelines_map_to_runs_with_known_conclusions() {
        let run = pipeline_run(&json!({
            "id": 91, "iid": 7, "status": "failed", "ref": "refs/merge-requests/13/head",
            "sha": "c92", "source": "merge_request_event", "web_url": "https://gitlab.com/p/-/pipelines/91"
        }));
        assert_eq!(run.conclusion_raw.as_deref(), Some("failure"));
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.pr_number, Some(13));
        let job = pipeline_job(
            &json!({"id": 1, "name": "deploy", "status": "manual", "stage": "deploy", "allow_failure": true}),
        );
        assert_eq!(job.status, RunStatus::Waiting);
        assert_eq!(job.conclusion, Some(CiState::Skipped));
        let jobs = latest_jobs(vec![
            json!({"id": 1, "name": "test", "stage": "test"}),
            json!({"id": 5, "name": "test", "stage": "test"}),
            json!({"id": 2, "name": "lint", "stage": "test"}),
        ]);
        assert_eq!(
            jobs.iter().map(|job| count(job, "id")).collect::<Vec<_>>(),
            [5, 2]
        );
    }
}
