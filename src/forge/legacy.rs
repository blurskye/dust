//! `Payload` → the gh-CLI JSON shapes `src/git/screen/github/view.rs` reads (FORGE.md §5.1).
//! The field sets are exactly what the old `gh … --json` calls requested, so the view keeps
//! working unchanged; DESIGN.md phase 4 moves the view onto the model and deletes this file.

use crate::forge::model::*;
use crate::forge::request::Payload;
use serde_json::Value;
use serde_json::json;

pub const ISSUE_LIST_FIELDS: &[&str] = &[
    "number",
    "title",
    "body",
    "state",
    "author",
    "labels",
    "updatedAt",
    "comments",
];
pub const PULL_REQUEST_LIST_FIELDS: &[&str] = &[
    "number",
    "title",
    "body",
    "state",
    "author",
    "labels",
    "updatedAt",
    "comments",
    "isDraft",
    "reviewDecision",
    "statusCheckRollup",
    "changedFiles",
];
pub const ISSUE_SEARCH_FIELDS: &[&str] = &[
    "number",
    "title",
    "body",
    "state",
    "author",
    "labels",
    "updatedAt",
    "commentsCount",
    "repository",
    "url",
];
pub const PULL_REQUEST_SEARCH_FIELDS: &[&str] = &[
    "number",
    "title",
    "body",
    "state",
    "author",
    "labels",
    "updatedAt",
    "commentsCount",
    "repository",
    "url",
    "isDraft",
];
pub const ISSUE_VIEW_FIELDS: &[&str] = &[
    "id",
    "number",
    "title",
    "body",
    "state",
    "author",
    "createdAt",
    "updatedAt",
    "assignees",
    "labels",
    "milestone",
    "projectItems",
    "closedByPullRequestsReferences",
];
pub const PULL_REQUEST_VIEW_FIELDS: &[&str] = &[
    "id",
    "number",
    "title",
    "body",
    "state",
    "author",
    "createdAt",
    "assignees",
    "labels",
    "milestone",
    "projectItems",
    "baseRefName",
    "headRefName",
    "headRefOid",
    "isDraft",
    "changedFiles",
    "commits",
    "files",
    "reviewDecision",
    "mergeStateStatus",
    "mergeable",
    "reviews",
    "statusCheckRollup",
    "closingIssuesReferences",
];
pub const RUN_LIST_FIELDS: &[&str] = &[
    "databaseId",
    "displayTitle",
    "workflowName",
    "status",
    "conclusion",
    "headBranch",
    "headSha",
    "event",
    "createdAt",
    "attempt",
    "workflowDatabaseId",
    "number",
    "url",
];
pub const RUN_VIEW_FIELDS: &[&str] = &[
    "databaseId",
    "displayTitle",
    "workflowName",
    "status",
    "conclusion",
    "headBranch",
    "headSha",
    "event",
    "attempt",
    "jobs",
    "workflowDatabaseId",
    "number",
    "url",
];
pub const REPOSITORY_FIELDS: &[&str] = &["nameWithOwner", "url", "defaultBranchRef"];

/// Which gh command's output a payload stands in for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    /// `gh repo view --json nameWithOwner,url,defaultBranchRef`
    Repository,
    /// `gh api repos/{o}/{r}` as the PR form reads it.
    RepositoryRest,
    /// `gh issue|pr list --json …`
    List(ItemKind),
    /// `gh search issues|prs --json …`
    Search(ItemKind),
    /// `gh issue|pr view --json …`
    Detail,
    /// `gh api …/timeline --paginate --slurp`
    Timeline,
    /// The paginated reviewThreads GraphQL pages.
    Threads,
    /// The paginated viewerViewedState GraphQL pages.
    Viewed,
    /// `gh api …/actions/workflows`
    Workflows,
    /// `gh run list --json …`
    Runs,
    /// `gh run view --json …`
    Run,
    /// Text: diffs, workflow YAML, `gh run view --job N --log`.
    Text,
}

/// The legacy JSON value and the raw text view.rs keeps beside it.
pub fn encode(shape: Shape, payload: &Payload) -> (Value, String) {
    let value = match (shape, payload) {
        (_, Payload::Text(text)) => return (Value::Null, text.clone()),
        (_, Payload::Log(log)) => return (Value::Null, log_text(log)),
        (Shape::Repository, Payload::Repo(repo)) => json!({
            "nameWithOwner": repo.full_name,
            "url": repo.web_url,
            "defaultBranchRef": { "name": repo.default_branch },
        }),
        (_, Payload::Repo(repo)) => {
            let mut value = json!({
                "full_name": repo.full_name,
                "default_branch": repo.default_branch,
                "html_url": repo.web_url,
                "fork": repo.is_fork,
            });
            if let Some(parent) = &repo.parent {
                value["parent"] = json!({
                    "full_name": parent,
                    "default_branch": repo.parent_default_branch.clone().unwrap_or_default(),
                });
            }
            value
        }
        (Shape::Search(kind), Payload::Rows(rows)) => {
            Value::Array(rows.items.iter().map(|row| search_row(row, kind)).collect())
        }
        (Shape::List(kind), Payload::Rows(rows)) => {
            Value::Array(rows.items.iter().map(|row| list_row(row, kind)).collect())
        }
        (_, Payload::Rows(rows)) => Value::Array(
            rows.items
                .iter()
                .map(|row| list_row(row, row.kind.unwrap_or(ItemKind::Issue)))
                .collect(),
        ),
        (_, Payload::Issue(issue)) => issue_view(issue),
        (_, Payload::PullRequest(pull)) => pull_request_view(pull),
        (_, Payload::Timeline(events)) => {
            json!([events.iter().map(timeline_event).collect::<Vec<_>>()])
        }
        (_, Payload::Threads(threads)) => json!([{
            "data": { "repository": { "pullRequest": { "reviewThreads": {
                "nodes": threads.iter().map(thread).collect::<Vec<_>>(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
            } } } }
        }]),
        (_, Payload::Viewed(files)) => json!([{
            "data": { "repository": { "pullRequest": { "files": {
                "nodes": files.iter().map(|(path, viewed)| json!({
                    "path": path,
                    "viewerViewedState": if *viewed { "VIEWED" } else { "UNVIEWED" },
                })).collect::<Vec<_>>(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
            } } } }
        }]),
        (_, Payload::Workflows(workflows)) => json!({
            "total_count": workflows.len(),
            "workflows": workflows.iter().map(|workflow| json!({
                "id": workflow_number(&workflow.id),
                "name": workflow.name,
                "path": workflow.path,
                "state": workflow.state,
                // Known only where the forge says (GitLab's sources); else read with `d`.
                "dispatchable": workflow.dispatchable,
            })).collect::<Vec<_>>(),
        }),
        (_, Payload::Runs(runs)) => Value::Array(runs.items.iter().map(run_row).collect()),
        (_, Payload::Run(detail)) => run_view(detail),
        (_, payload) => serde_json::to_value(payload).unwrap_or_default(),
    };
    let text = value.to_string();
    (value, text)
}

fn number_id(id: &ItemId) -> Value {
    id.0.parse::<u64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(id.0.clone()))
}

/// Workflow ids the legacy view keys by number: Gitea and Forgejo name a workflow by its file
/// (`ci.yml`), GitLab by its pipeline source (`push`). Such an id is shown to the view as a
/// stable number with bit 52 set (above any real GitHub workflow id, still exact in an f64),
/// and `workflow_id` maps that number back when the view sends it in a `Read` or `Write`.
fn workflow_number(id: &ItemId) -> Value {
    if let Ok(number) = id.0.parse::<u64>() {
        return Value::from(number);
    }
    if id.0.is_empty() {
        return Value::from(0);
    }
    // FNV-1a: stable across runs, so cached runs keep their workflow.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.0.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let alias = (hash & ((1 << 52) - 1)) | (1 << 52);
    aliases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(alias, id.0.clone());
    Value::from(alias)
}

fn aliases() -> &'static std::sync::Mutex<std::collections::HashMap<u64, String>> {
    static ALIASES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, String>>> =
        std::sync::OnceLock::new();
    ALIASES.get_or_init(Default::default)
}

/// The provider's own workflow id for one the legacy view sent (see `workflow_number`).
pub fn workflow_id(id: &ItemId) -> ItemId {
    id.0.parse::<u64>()
        .ok()
        .filter(|number| *number >= 1 << 52)
        .and_then(|number| {
            aliases()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&number)
                .cloned()
        })
        .map(ItemId)
        .unwrap_or_else(|| id.clone())
}

fn author(user: &User) -> Value {
    if user.bot {
        // REST says `type`, GraphQL `__typename`; one value serves both shapes.
        return json!({ "login": user.login, "type": "Bot", "__typename": "Bot" });
    }
    json!({ "login": user.login })
}

/// REST's reaction rollup on a comment (`{"+1": 2, "heart": 1, "total_count": 3}`).
fn reaction_rollup(reactions: &[Reaction]) -> Value {
    let mut value = serde_json::Map::new();
    for reaction in reactions {
        value.insert(reaction.content.clone(), reaction.count.into());
    }
    value.insert(
        "total_count".into(),
        reactions.iter().map(|reaction| reaction.count).sum::<u64>().into(),
    );
    Value::Object(value)
}

/// GraphQL's `reactionGroups` for a review comment.
fn reaction_groups(reactions: &[Reaction]) -> Value {
    let word = |content: &str| match content {
        "+1" => "THUMBS_UP".to_owned(),
        "-1" => "THUMBS_DOWN".to_owned(),
        other => other.to_ascii_uppercase(),
    };
    reactions
        .iter()
        .map(|reaction| json!({ "content": word(&reaction.content), "users": { "totalCount": reaction.count } }))
        .collect()
}

fn state(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Open | Lifecycle::Draft => "OPEN",
        Lifecycle::Merged => "MERGED",
        Lifecycle::Closed | Lifecycle::ClosedNotPlanned => "CLOSED",
    }
}

fn decision(review: Option<ReviewState>) -> &'static str {
    match review {
        Some(ReviewState::Approved) => "APPROVED",
        Some(ReviewState::ChangesRequested) => "CHANGES_REQUESTED",
        Some(ReviewState::Requested) => "REVIEW_REQUIRED",
        _ => "",
    }
}

fn review_word(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "APPROVED",
        ReviewState::ChangesRequested => "CHANGES_REQUESTED",
        ReviewState::Dismissed => "DISMISSED",
        ReviewState::Pending => "PENDING",
        ReviewState::Requested | ReviewState::Commented => "COMMENTED",
        // gh passed unknown states through; view.rs shows them as "unknown".
        ReviewState::NotRequested => "",
    }
}

pub fn ci_word(state: CiState) -> &'static str {
    match state {
        CiState::Passed => "SUCCESS",
        CiState::Failed => "FAILURE",
        CiState::Pending => "PENDING",
        CiState::Skipped => "SKIPPED",
        CiState::Cancelled => "CANCELLED",
        CiState::ActionRequired => "ACTION_REQUIRED",
        CiState::Unknown => "",
    }
}

fn labels(labels: &[Label]) -> Value {
    labels
        .iter()
        .map(|label| match &label.color {
            Some(color) => json!({ "name": label.name, "color": color }),
            None => json!({ "name": label.name }),
        })
        .collect()
}

fn common_row(row: &ItemRow) -> serde_json::Map<String, Value> {
    let mut value = serde_json::Map::new();
    value.insert("number".into(), row.number.into());
    value.insert("title".into(), row.title.clone().into());
    if let Some(body) = &row.body_preview {
        value.insert("body".into(), body.clone().into());
    }
    value.insert("state".into(), state(row.lifecycle).into());
    value.insert("author".into(), author(&row.author));
    value.insert("labels".into(), labels(&row.labels));
    value.insert("updatedAt".into(), row.updated.as_str().into());
    value.insert("dusty".into(), row_extension(row));
    value
}

/// Facts the gh `--json` field sets never carried, for the typed forge records
/// (`forge::records`): why an item closed, and a pull request's line counts. One key so
/// the gh field-set parity stays exact.
fn row_extension(row: &ItemRow) -> Value {
    let mut extension = serde_json::Map::new();
    if row.lifecycle == Lifecycle::ClosedNotPlanned {
        extension.insert("stateReason".into(), "NOT_PLANNED".into());
    }
    if let Some(additions) = row.additions {
        extension.insert("additions".into(), additions.into());
    }
    if let Some(deletions) = row.deletions {
        extension.insert("deletions".into(), deletions.into());
    }
    if let Some(review) = row.review {
        extension.insert("review".into(), review_word(review).into());
        if review == ReviewState::Requested {
            extension.insert("reviewRequested".into(), true.into());
        }
    }
    if let Some(ci) = row.ci {
        extension.insert("ci".into(), ci_word(ci).into());
    }
    if let Some(created) = &row.created {
        extension.insert("created".into(), created.as_str().into());
    }
    if let Some(linked) = row.linked {
        extension.insert("linked".into(), linked.into());
    }
    if let Some(tally) = row.review_tally {
        extension.insert(
            "reviews".into(),
            json!({
                "approved": tally.approved,
                "changesRequested": tally.changes_requested,
                "pending": tally.pending,
            }),
        );
    }
    Value::Object(extension)
}

fn list_row(row: &ItemRow, kind: ItemKind) -> Value {
    let mut value = common_row(row);
    // `gh … list --json comments` returned up to 100 comment objects; view.rs counts them.
    value.insert(
        "comments".into(),
        Value::Array(vec![json!({}); row.comments.min(100) as usize]),
    );
    if kind == ItemKind::PullRequest {
        value.insert("isDraft".into(), row.draft.into());
        value.insert("reviewDecision".into(), decision(row.review).into());
        let mut checks = Vec::new();
        for (state, count) in &row.ci_counts {
            for _ in 0..*count {
                checks.push(json!({ "conclusion": ci_word(*state) }));
            }
        }
        value.insert("statusCheckRollup".into(), Value::Array(checks));
        if let Some(files) = row.changed_files {
            value.insert("changedFiles".into(), files.into());
        }
    }
    Value::Object(value)
}

fn search_row(row: &ItemRow, kind: ItemKind) -> Value {
    let mut value = common_row(row);
    value.insert("commentsCount".into(), row.comments.into());
    value.insert(
        "repository".into(),
        json!({ "nameWithOwner": row.repo.clone().unwrap_or_default() }),
    );
    value.insert("url".into(), row.web_url.clone().into());
    if kind == ItemKind::PullRequest {
        value.insert("isDraft".into(), row.draft.into());
    }
    Value::Object(value)
}

fn milestone(milestone: &Option<Milestone>) -> Value {
    milestone
        .as_ref()
        .map(|milestone| json!({ "title": milestone.title }))
        .unwrap_or(Value::Null)
}

fn link(link: &LinkRef) -> Value {
    json!({
        "number": link.number,
        "title": link.title,
        "url": link.web_url.clone().unwrap_or_default(),
        "repository": { "nameWithOwner": link.repo.clone().unwrap_or_default() },
    })
}

fn issue_view(issue: &Issue) -> Value {
    with_reactions(issue_fields(issue), &issue.reactions)
}

/// The item's REST reaction rollup, where it has reactions.
fn with_reactions(mut value: Value, reactions: &[Reaction]) -> Value {
    if !reactions.is_empty() {
        value["reactions"] = reaction_rollup(reactions);
    }
    value
}

fn issue_fields(issue: &Issue) -> Value {
    json!({
        "id": issue.row.id.0,
        "number": issue.row.number,
        "title": issue.row.title,
        "body": issue.body,
        "state": state(issue.row.lifecycle),
        "author": author(&issue.row.author),
        "createdAt": issue.created.as_str(),
        "updatedAt": issue.row.updated.as_str(),
        "assignees": issue.assignees.iter().map(author).collect::<Vec<_>>(),
        "labels": labels(&issue.row.labels),
        "milestone": milestone(&issue.milestone),
        "projectItems": issue.projects.iter().map(|title| json!({ "title": title })).collect::<Vec<_>>(),
        "closedByPullRequestsReferences": issue.linked_prs.iter().map(link).collect::<Vec<_>>(),
        "dusty": {
            "stateReason": match issue.closed_reason {
                Some(ClosedReason::NotPlanned) => "NOT_PLANNED",
                Some(ClosedReason::Duplicate) => "DUPLICATE",
                Some(ClosedReason::Completed) => "COMPLETED",
                None if issue.row.lifecycle == Lifecycle::ClosedNotPlanned => "NOT_PLANNED",
                None => "",
            },
            "comments": issue.row.comments,
        },
    })
}

fn merge_state(mergeable: Mergeable) -> &'static str {
    match mergeable {
        Mergeable::Clean => "CLEAN",
        Mergeable::Behind => "BEHIND",
        Mergeable::Blocked => "BLOCKED",
        Mergeable::Conflicting => "DIRTY",
        Mergeable::Draft => "DRAFT",
        Mergeable::HasHooks => "HAS_HOOKS",
        Mergeable::Unstable => "UNSTABLE",
        Mergeable::Unknown => "UNKNOWN",
    }
}

fn check(check: &Check) -> Value {
    let word = check
        .raw_state
        .clone()
        .unwrap_or_else(|| ci_word(check.state).into());
    json!({
        "__typename": "CheckRun",
        "name": check.name,
        "workflowName": check.workflow.clone().unwrap_or_default(),
        "conclusion": word,
        "status": if check.state == CiState::Pending { "IN_PROGRESS" } else { "COMPLETED" },
        "detailsUrl": check.details_url.clone().unwrap_or_default(),
        "startedAt": check.started.as_ref().map(Timestamp::as_str).unwrap_or_default(),
        "completedAt": check.completed.as_ref().map(Timestamp::as_str).unwrap_or_default(),
        // The run as the API reads it, where the link names it otherwise: Gitea's status
        // links its run by its index in the repository (`…/actions/runs/6`), not its id.
        "dustyRun": api_run(check),
    })
}

/// The id the API reads the check's run by, when its link names an Actions run.
fn api_run(check: &Check) -> Value {
    let linked = check.details_url.as_deref().is_some_and(|url| url.contains("/actions/runs/"));
    check
        .run_link
        .as_ref()
        .filter(|_| linked)
        .and_then(|link| link.run.0.parse::<u64>().ok())
        .map_or(Value::Null, Value::from)
}

fn pull_request_view(pull: &PullRequest) -> Value {
    with_reactions(pull_request_fields(pull), &pull.reactions)
}

fn pull_request_fields(pull: &PullRequest) -> Value {
    let row = &pull.row;
    json!({
        "id": row.id.0,
        "number": row.number,
        "title": row.title,
        "body": pull.body,
        "state": state(row.lifecycle),
        "author": author(&row.author),
        "createdAt": pull.created.as_str(),
        "assignees": pull.assignees.iter().map(author).collect::<Vec<_>>(),
        "labels": labels(&row.labels),
        "milestone": milestone(&pull.milestone),
        "projectItems": pull.projects.iter().map(|title| json!({ "title": title })).collect::<Vec<_>>(),
        "baseRefName": pull.base.branch,
        "headRefName": pull.head.branch,
        "headRefOid": pull.head.sha,
        "isDraft": row.draft,
        "changedFiles": row.changed_files.unwrap_or(pull.files.len() as u64),
        "commits": pull.commits.iter().map(|commit| json!({
            "oid": commit.sha,
            "messageHeadline": commit.headline,
            "messageBody": commit.body,
            "committedDate": commit.committed.as_str(),
            "authors": commit.author.iter().map(author).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "files": pull.files.iter().map(|file| json!({
            "path": file.path,
            "additions": file.additions,
            "deletions": file.deletions,
        })).collect::<Vec<_>>(),
        "reviewDecision": decision(row.review),
        "mergeStateStatus": merge_state(pull.mergeable),
        "mergeable": pull.fingerprint.mergeable,
        "reviews": pull.reviews.iter().map(|review| json!({
            "id": review.id.0,
            "author": author(&review.author),
            "state": review_word(review.state),
            "body": review.body,
            "submittedAt": review.submitted.as_str(),
        })).collect::<Vec<_>>(),
        "statusCheckRollup": pull.checks.iter().map(check).collect::<Vec<_>>(),
        "closingIssuesReferences": pull.linked_issues.iter().map(link).collect::<Vec<_>>(),
        "dusty": {
            "reviewRequests": pull.reviewers.iter()
                .filter(|slot| slot.state == ReviewState::Requested)
                .map(|slot| slot.reviewer.clone())
                .collect::<Vec<_>>(),
            "additions": row.additions.unwrap_or_else(|| pull.files.iter().map(|file| file.additions).sum()),
            "deletions": row.deletions.unwrap_or_else(|| pull.files.iter().map(|file| file.deletions).sum()),
            "updatedAt": row.updated.as_str(),
            "comments": row.comments,
            "headRepository": pull.head.repo.clone().unwrap_or_default(),
            "files": pull.files.iter().map(|file| json!({
                "path": file.path,
                "status": format!("{:?}", file.status),
                "oldPath": file.old_path.clone().unwrap_or_default(),
            })).collect::<Vec<_>>(),
            "required": pull.checks.iter().filter(|check| check.required).map(|check| check.name.clone()).collect::<Vec<_>>(),
        },
    })
}

fn event(kind: &str, by: &Actor) -> serde_json::Map<String, Value> {
    let mut value = serde_json::Map::new();
    value.insert("event".into(), kind.into());
    value.insert("actor".into(), author(&by.actor));
    value.insert("created_at".into(), by.at.as_str().into());
    value
}

/// The REST timeline shape (`event`, `actor`/`user`, `created_at`, lowercase review state).
fn timeline_event(timeline: &TimelineEvent) -> Value {
    let value = match timeline {
        TimelineEvent::Commented(comment) => {
            let mut value = serde_json::Map::new();
            value.insert("event".into(), "commented".into());
            value.insert("user".into(), author(&comment.author));
            value.insert("body".into(), comment.body.clone().into());
            value.insert("created_at".into(), comment.created.as_str().into());
            value.insert("node_id".into(), comment.id.0.clone().into());
            if let Some(association) = &comment.association {
                value.insert("author_association".into(), association.clone().into());
            }
            if let Some(edited) = &comment.edited {
                value.insert("updated_at".into(), edited.as_str().into());
            }
            if !comment.reactions.is_empty() {
                value.insert("reactions".into(), reaction_rollup(&comment.reactions));
            }
            value
        }
        TimelineEvent::Reviewed(review) => {
            let mut value = serde_json::Map::new();
            value.insert("event".into(), "reviewed".into());
            value.insert("user".into(), author(&review.author));
            value.insert(
                "state".into(),
                review_word(review.state).to_ascii_lowercase().into(),
            );
            value.insert("body".into(), review.body.clone().into());
            value.insert("created_at".into(), review.submitted.as_str().into());
            value.insert("node_id".into(), review.id.0.clone().into());
            value
        }
        TimelineEvent::Labeled { label, by } | TimelineEvent::Unlabeled { label, by } => {
            let kind = if matches!(timeline, TimelineEvent::Labeled { .. }) {
                "labeled"
            } else {
                "unlabeled"
            };
            let mut value = event(kind, by);
            value.insert("label".into(), json!({ "name": label }));
            value
        }
        TimelineEvent::Assigned { assignee, by } | TimelineEvent::Unassigned { assignee, by } => {
            let kind = if matches!(timeline, TimelineEvent::Assigned { .. }) {
                "assigned"
            } else {
                "unassigned"
            };
            let mut value = event(kind, by);
            value.insert("assignee".into(), json!({ "login": assignee }));
            value
        }
        TimelineEvent::Closed { by } => event("closed", by),
        TimelineEvent::Reopened { by } => event("reopened", by),
        TimelineEvent::Merged { sha, by } => {
            let mut value = event("merged", by);
            value.insert("commit_id".into(), sha.clone().into());
            value
        }
        TimelineEvent::Committed(commit) => {
            let mut value = serde_json::Map::new();
            value.insert("event".into(), "committed".into());
            value.insert("sha".into(), commit.sha.clone().into());
            value.insert("message".into(), commit.body.clone().into());
            if let Some(user) = &commit.author {
                let mut who = author(user);
                // REST's commit author is a name (no login) when the forge has no account for it.
                if let Some(name) = &user.name {
                    who["name"] = name.clone().into();
                }
                value.insert("author".into(), who);
            }
            value.insert("created_at".into(), commit.committed.as_str().into());
            value
        }
        TimelineEvent::ForcePushed { from, to, by } => {
            let mut value = event("head_ref_force_pushed", by);
            value.insert("before".into(), from.clone().into());
            value.insert("after".into(), to.clone().into());
            value
        }
        TimelineEvent::CrossReferenced { link, by } => {
            let mut value = event("cross-referenced", by);
            let mut issue = json!({
                "number": link.number,
                "title": link.title,
                "html_url": link.web_url.clone().unwrap_or_default(),
                "repository": { "full_name": link.repo.clone().unwrap_or_default() },
            });
            if link.kind == Some(ItemKind::PullRequest) {
                issue["pull_request"] = json!({});
            }
            value.insert("source".into(), json!({ "type": "issue", "issue": issue }));
            value
        }
        TimelineEvent::Renamed { from, to, by } => {
            let mut value = event("renamed", by);
            value.insert("rename".into(), json!({ "from": from, "to": to }));
            value
        }
        TimelineEvent::ReviewRequested { reviewer, by } => {
            let mut value = event("review_requested", by);
            value.insert("requested_reviewer".into(), json!({ "login": reviewer }));
            value
        }
        TimelineEvent::Milestoned { milestone, by } | TimelineEvent::Demilestoned { milestone, by } => {
            let kind = if matches!(timeline, TimelineEvent::Milestoned { .. }) {
                "milestoned"
            } else {
                "demilestoned"
            };
            let mut value = event(kind, by);
            value.insert("milestone".into(), json!({ "title": milestone }));
            value
        }
        TimelineEvent::Other { kind, by } => event(kind, by),
        TimelineEvent::Detailed { kind, detail, by } => {
            let mut value = event(kind, by);
            for (path, text) in detail {
                match path.split_once('.') {
                    Some((outer, inner)) => {
                        let nested = value.entry(outer).or_insert_with(|| json!({}));
                        nested[inner] = text.clone().into();
                    }
                    None => {
                        value.insert(path.clone(), text.clone().into());
                    }
                }
            }
            value
        }
    };
    Value::Object(value)
}

fn thread(thread: &ReviewThread) -> Value {
    let mut value = json!({
        "id": thread.id.0,
        "isResolved": thread.resolved,
        "path": thread.path,
        "line": thread.line,
        "originalLine": thread.original_line,
        "diffSide": thread.side.as_github(),
        "comments": {
            "totalCount": thread.total_comments.unwrap_or(thread.comments.len() as u64),
            "nodes": thread.comments.iter().map(|comment| {
                let position = comment.position.clone().unwrap_or_default();
                let mut node = json!({
                    "id": comment.id.0,
                    "createdAt": comment.created.as_str(),
                    "author": author(&comment.author),
                    "body": comment.body,
                    "path": position.path,
                    "line": position.line,
                    "originalLine": position.original_line,
                    "commit": { "oid": position.commit.unwrap_or_default() },
                });
                // GitHub repeats the hunk on every comment of a thread.
                if let Some(hunk) = &thread.diff_hunk {
                    node["diffHunk"] = hunk.clone().into();
                }
                if let Some(association) = &comment.association {
                    node["authorAssociation"] = association.clone().into();
                }
                if let Some(edited) = &comment.edited {
                    node["lastEditedAt"] = edited.as_str().into();
                }
                if !comment.reactions.is_empty() {
                    node["reactionGroups"] = reaction_groups(&comment.reactions);
                }
                node
            }).collect::<Vec<_>>(),
        },
    });
    // A multi-line thread's first line (GitHub's `startLine`); one-line threads leave it out.
    if let Some(start) = thread.start_line {
        value["startLine"] = start.into();
    }
    value
}

fn status_word(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::InProgress => "in_progress",
        RunStatus::Completed => "completed",
        RunStatus::Waiting => "waiting",
        RunStatus::Requested => "requested",
        RunStatus::Pending => "pending",
    }
}

fn run_fields(run: &Run) -> serde_json::Map<String, Value> {
    let mut value = serde_json::Map::new();
    value.insert("databaseId".into(), number_id(&run.id));
    value.insert("displayTitle".into(), run.title.clone().into());
    value.insert(
        "workflowName".into(),
        run.workflow
            .as_ref()
            .map(|workflow| workflow.name.clone())
            .unwrap_or_default()
            .into(),
    );
    value.insert("status".into(), status_word(run.status).into());
    value.insert(
        "conclusion".into(),
        run.conclusion_raw.clone().unwrap_or_default().into(),
    );
    value.insert("headBranch".into(), run.branch.clone().into());
    value.insert("headSha".into(), run.sha.clone().into());
    value.insert("event".into(), run.event.clone().into());
    value.insert("attempt".into(), run.attempt.into());
    value.insert(
        "workflowDatabaseId".into(),
        run.workflow
            .as_ref()
            .map(|workflow| workflow_number(&workflow.id))
            .unwrap_or(Value::from(0)),
    );
    value.insert("number".into(), run.number.into());
    value.insert("url".into(), run.web_url.clone().into());
    value.insert("dusty".into(), run_extension(run));
    value
}

/// What the Actions table and sheets (§3.37) need that gh's run fields never carried: the
/// duration, the pull request, approval, who started it and how many attempts there are. One
/// key, as for items (`row_extension`), so the gh field-set parity stays exact.
fn run_extension(run: &Run) -> Value {
    let mut extension = serde_json::Map::new();
    if let Some(duration) = run.duration {
        extension.insert("durationSeconds".into(), duration.as_secs().into());
    }
    if let Some(pr) = run.pr_number {
        extension.insert("prNumber".into(), pr.into());
    }
    extension.insert("needsApproval".into(), run.needs_approval.into());
    if !run.actor.is_empty() {
        extension.insert("actor".into(), run.actor.clone().into());
    }
    extension.insert("attempts".into(), run.attempts.into());
    Value::Object(extension)
}

fn run_row(run: &Run) -> Value {
    let mut value = run_fields(run);
    value.insert("createdAt".into(), run.created.as_str().into());
    Value::Object(value)
}

fn time(value: &Option<Timestamp>) -> &str {
    value.as_ref().map(Timestamp::as_str).unwrap_or_default()
}

fn run_view(detail: &RunDetail) -> Value {
    let mut value = run_fields(&detail.run);
    value.insert(
        "jobs".into(),
        detail
            .jobs
            .iter()
            .map(|job| {
                let steps = match &job.steps {
                    Steps::Known(steps) => steps
                        .iter()
                        .map(|step| {
                            json!({
                                "name": step.name,
                                "number": step.number,
                                "status": status_word(step.status),
                                "conclusion": step.conclusion_raw.clone().unwrap_or_default(),
                                "startedAt": time(&step.started),
                                "completedAt": time(&step.completed),
                            })
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                json!({
                    "databaseId": number_id(&job.id),
                    "name": job.name,
                    "status": status_word(job.status),
                    "conclusion": job.conclusion_raw.clone().unwrap_or_default(),
                    "startedAt": time(&job.started),
                    "completedAt": time(&job.completed),
                    "url": job.web_url,
                    "steps": steps,
                })
            })
            .collect(),
    );
    if let Some(failure) = &detail.failure_summary {
        value.insert(
            "failureSummary".into(),
            json!({"job": number_id(&failure.job), "line": failure.line}),
        );
    }
    Value::Object(value)
}

/// `gh run view --job N --log` prints `job<TAB>step<TAB>line`; REST logs are plain lines.
pub fn log_text(log: &Log) -> String {
    match log.format {
        LogFormat::GhTabbed => log.text.clone(),
        LogFormat::Plain | LogFormat::GitLabSections => {
            let mut text = String::with_capacity(log.text.len() + log.text.len() / 4);
            for line in log.text.lines() {
                text.push_str("job\tUNKNOWN STEP\t");
                text.push_str(line);
                text.push('\n');
            }
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::request::PageOf;

    #[test]
    fn rows_carry_exactly_the_gh_list_fields() {
        let row = ItemRow {
            kind: Some(ItemKind::PullRequest),
            number: 9,
            title: "t".into(),
            body_preview: Some("b".into()),
            comments: 120,
            ci_counts: vec![(CiState::Passed, 2), (CiState::Failed, 1)],
            changed_files: Some(2),
            ..ItemRow::default()
        };
        let rows = Payload::Rows(PageOf {
            items: vec![row],
            ..PageOf::default()
        });
        let (value, _) = encode(Shape::List(ItemKind::PullRequest), &rows);
        let keys = value[0]
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| *key != "dusty")
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let expected = PULL_REQUEST_LIST_FIELDS
            .iter()
            .map(|key| key.to_string())
            .collect();
        assert_eq!(keys, expected);
        assert_eq!(value[0]["comments"].as_array().unwrap().len(), 100);
        assert_eq!(value[0]["statusCheckRollup"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn string_workflow_ids_round_trip_through_numbers() {
        let file = ItemId::new("ci.yml");
        let number = workflow_number(&file).as_u64().unwrap();
        assert!(number >= 1 << 52);
        assert_eq!(workflow_number(&file).as_u64(), Some(number));
        assert_eq!(workflow_id(&ItemId::new(number)), file);
        assert_eq!(workflow_id(&ItemId::new(31)), ItemId::new(31));
        assert_eq!(workflow_number(&ItemId::new(31)), Value::from(31));
    }

    #[test]
    fn plain_logs_gain_the_gh_columns() {
        let log = Log::new(
            ItemId::new(42),
            "2026-01-01T00:00:00Z a\n2026-01-01T00:00:01Z b".into(),
            LogFormat::Plain,
            false,
        );
        assert_eq!(
            log_text(&log),
            "job\tUNKNOWN STEP\t2026-01-01T00:00:00Z a\njob\tUNKNOWN STEP\t2026-01-01T00:00:01Z b\n"
        );
    }
}
