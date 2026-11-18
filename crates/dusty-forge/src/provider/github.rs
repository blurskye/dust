//! GitHub (github.com, `*.ghe.com`, GHES) over REST v3 and GraphQL: a slim
//! GraphQL search for lists, one GraphQL document per detail, REST for timelines, diffs,
//! Actions and every write except the thread/viewed/subscription mutations GitHub only offers
//! in GraphQL. Without a credential `anonymous.rs` answers the GraphQL reads over REST.

mod anonymous;

use super::Conditional;
use super::Fetched;
use super::ForgeContext;
use super::Plan;
use super::Provider;
use super::ProviderKind;
use super::RepoRef;
use super::common::*;
use crate::capability::Capabilities;
use crate::capability::GITHUB_TERMS;
use crate::capability::Terms;
use crate::model::*;
use crate::request::*;
use crate::transport::Accept;
use crate::transport::ApiRequest;
use crate::transport::Method;
use crate::transport::Target;
use serde_json::Value;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub struct GitHub;

/// GitHub's page size for REST and GraphQL connections.
const PAGE: u32 = 100;
/// Bounded fan-out for paginated detail reads (timeline, threads, files).
const MAX_PAGES: usize = 30;

const ROW_FIELDS: &str = "id number title body state url createdAt updatedAt author { login } \
labels(first: 20) { nodes { name color } } comments { totalCount } repository { nameWithOwner }";

const PR_ROW_FIELDS: &str = "isDraft reviewDecision additions deletions changedFiles headRefName \
closingIssuesReferences { totalCount } \
latestOpinionatedReviews(first: 20) { nodes { state } } reviewRequests { totalCount } \
commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 1) { \
checkRunCountsByState { state count } statusContextCountsByState { state count } } } } } }";

/// The list read, and with `counted` the same query's other state counted alongside it
/// (`other`, the list header's `Open · Closed` counts in one request).
fn list_document(kind: ItemKind, counted: bool) -> String {
    let (name, fields) = match kind {
        ItemKind::Issue => (
            "IssueList",
            format!(
                "... on Issue {{ {ROW_FIELDS} stateReason closedByPullRequestsReferences {{ totalCount }} }}"
            ),
        ),
        ItemKind::PullRequest => (
            "PullRequestList",
            format!("... on PullRequest {{ {ROW_FIELDS} {PR_ROW_FIELDS} }}"),
        ),
    };
    let (other_variable, other) = if counted {
        (
            ", $other: String!",
            " other: search(type: ISSUE, query: $other, first: 1) { issueCount }",
        )
    } else {
        ("", "")
    };
    format!(
        "query {name}($q: String!, $first: Int!, $after: String{other_variable}) {{ search(type: ISSUE, query: $q, first: $first, after: $after) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ __typename {fields} }} }}{other} }}"
    )
}

/// The list in its other state (open ↔ closed), whose count the list header shows.
fn other_state(filter: &Filter) -> Option<Filter> {
    let state = match filter.state {
        StateFilter::Open => StateFilter::Closed,
        StateFilter::Closed | StateFilter::Merged => StateFilter::Open,
        StateFilter::All => return None,
    };
    Some(Filter {
        state,
        query: filter.query.clone(),
    })
}

const ISSUE_DOCUMENT: &str = "query IssueDetail($owner: String!, $name: String!, $number: Int!) { \
repository(owner: $owner, name: $name) { issue(number: $number) { \
id number title body state stateReason url createdAt updatedAt author { login } \
reactionGroups { content users { totalCount } } \
assignees(first: 50) { nodes { login } } labels(first: 50) { nodes { name color } } \
milestone { number title dueOn state } projectItems(first: 20) { nodes { project { title } } } \
viewerSubscription comments { totalCount } \
closedByPullRequestsReferences(first: 20, includeClosedPrs: true) { nodes { number title url repository { nameWithOwner } } } \
} } }";

const PULL_REQUEST_DOCUMENT: &str = "query PullRequestDetail($owner: String!, $name: String!, $number: Int!) { \
repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
id number title body state isDraft url createdAt updatedAt author { login } \
reactionGroups { content users { totalCount } } \
baseRefName headRefName headRefOid baseRefOid headRepository { nameWithOwner } \
mergeable mergeStateStatus reviewDecision additions deletions changedFiles comments { totalCount } \
assignees(first: 50) { nodes { login } } labels(first: 50) { nodes { name color } } \
milestone { number title dueOn state } projectItems(first: 20) { nodes { project { title } } } \
viewerSubscription \
reviewRequests(first: 50) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { slug } } } } \
reviews(last: 50) { nodes { id author { login } state body submittedAt } } \
commits(first: 100) { nodes { commit { oid messageHeadline messageBody committedDate author { user { login } } } } } \
files(first: 100) { pageInfo { hasNextPage endCursor } nodes { path additions deletions changeType } } \
closingIssuesReferences(first: 20) { nodes { number title url repository { nameWithOwner } } } \
latest: commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 100) { nodes { __typename \
... on CheckRun { name status conclusion startedAt completedAt detailsUrl checkSuite { workflowRun { databaseId workflow { name } } } } \
... on StatusContext { context state targetUrl createdAt } } } } } } } \
} } }";

const FILES_DOCUMENT: &str = "query PullRequestFiles($owner: String!, $name: String!, $number: Int!, $after: String) { \
repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
files(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { path additions deletions changeType viewerViewedState } } } } }";

const THREADS_DOCUMENT: &str = "query ReviewThreads($owner: String!, $name: String!, $number: Int!, $after: String) { \
repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { \
id isResolved path line originalLine startLine diffSide \
comments(first: 100) { totalCount nodes { id author { login __typename } authorAssociation body path line originalLine \
createdAt lastEditedAt diffHunk commit { oid } reactionGroups { content users { totalCount } } } } } } } } }";

const FINGERPRINT_DOCUMENT: &str = "query PullRequestFingerprint($owner: String!, $name: String!, $number: Int!) { \
repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
state mergeable headRefOid updatedAt commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } } } }";

const REPLY_DOCUMENT: &str = "mutation AddThreadReply($thread: ID!, $body: String!) { \
addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $thread, body: $body }) { comment { id url } } }";

const RESOLVE_DOCUMENT: &str = "mutation ResolveThread($thread: ID!) { \
resolveReviewThread(input: { threadId: $thread }) { thread { id isResolved } } }";

const UNRESOLVE_DOCUMENT: &str = "mutation UnresolveThread($thread: ID!) { \
unresolveReviewThread(input: { threadId: $thread }) { thread { id isResolved } } }";

const VIEWED_DOCUMENT: &str = "mutation MarkFileViewed($pullRequest: ID!, $path: String!) { \
markFileAsViewed(input: { pullRequestId: $pullRequest, path: $path }) { pullRequest { id } } }";

const UNVIEWED_DOCUMENT: &str = "mutation UnmarkFileViewed($pullRequest: ID!, $path: String!) { \
unmarkFileAsViewed(input: { pullRequestId: $pullRequest, path: $path }) { pullRequest { id } } }";

const SUBSCRIPTION_DOCUMENT: &str = "mutation UpdateSubscription($subject: ID!, $state: SubscriptionState!) { \
updateSubscription(input: { subscribableId: $subject, state: $state }) { subscribable { id } } }";

fn nodes(value: &Value) -> &[Value] {
    value["nodes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn user(value: &Value) -> User {
    User {
        bot: is_bot(value),
        ..User::login(value["login"].as_str().unwrap_or_default())
    }
}

/// REST's reaction rollup (`{"+1": 2, "heart": 1, "total_count": 3, …}`) or GraphQL's
/// `reactionGroups`, as GitHub's REST names in GitHub's order, without the empty ones.
fn reactions(value: &Value) -> Vec<Reaction> {
    const NAMES: [(&str, &str); 8] = [
        ("+1", "THUMBS_UP"),
        ("-1", "THUMBS_DOWN"),
        ("laugh", "LAUGH"),
        ("hooray", "HOORAY"),
        ("confused", "CONFUSED"),
        ("heart", "HEART"),
        ("rocket", "ROCKET"),
        ("eyes", "EYES"),
    ];
    let groups = value["reactionGroups"].as_array();
    NAMES
        .iter()
        .map(|(rest, graphql)| {
            let count = match groups {
                Some(groups) => groups
                    .iter()
                    .find(|group| group["content"] == *graphql)
                    .and_then(|group| group["users"]["totalCount"].as_u64())
                    .unwrap_or(0),
                None => value["reactions"][*rest].as_u64().unwrap_or(0),
            };
            Reaction {
                content: (*rest).to_owned(),
                count,
            }
        })
        .filter(|reaction| reaction.count > 0)
        .collect()
}

fn labels(value: &Value) -> Vec<Label> {
    let list = value["nodes"].as_array().or_else(|| value.as_array());
    list.into_iter()
        .flatten()
        .map(|label| Label {
            id: None,
            name: text(label, "name"),
            color: optional(label, "color"),
        })
        .collect()
}

fn milestone(value: &Value) -> Option<Milestone> {
    value.is_object().then(|| Milestone {
        id: value["number"].as_u64().map(ItemId::new),
        title: text(value, "title"),
        due: optional_time(value, "dueOn").or_else(|| optional_time(value, "due_on")),
        open: value["state"]
            .as_str()
            .is_none_or(|state| state.eq_ignore_ascii_case("open")),
    })
}

/// CheckRun conclusions/status and StatusContext states, as GitHub spells them.
pub fn ci_state(word: &str) -> CiState {
    match word.to_ascii_uppercase().as_str() {
        "SUCCESS" => CiState::Passed,
        "NEUTRAL" | "SKIPPED" => CiState::Skipped,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "STALE" => CiState::Failed,
        "CANCELLED" => CiState::Cancelled,
        "ACTION_REQUIRED" => CiState::ActionRequired,
        "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED"
        | "PENDING_DEPLOYMENT" => CiState::Pending,
        _ => CiState::Unknown,
    }
}

fn review_state(word: &str) -> ReviewState {
    match word.to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        "COMMENTED" => ReviewState::Commented,
        // A state this client does not know: not a comment, and not a verdict either.
        _ => ReviewState::NotRequested,
    }
}

fn review_decision(word: &str) -> Option<ReviewState> {
    match word {
        "APPROVED" => Some(ReviewState::Approved),
        "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewState::Requested),
        _ => None,
    }
}

fn lifecycle(value: &Value) -> Lifecycle {
    match text(value, "state").to_ascii_uppercase().as_str() {
        "MERGED" => Lifecycle::Merged,
        "CLOSED" if text(value, "stateReason").eq_ignore_ascii_case("NOT_PLANNED") => {
            Lifecycle::ClosedNotPlanned
        }
        "CLOSED" => Lifecycle::Closed,
        _ if value["isDraft"].as_bool() == Some(true) => Lifecycle::Draft,
        _ => Lifecycle::Open,
    }
}

fn mergeable(word: &str) -> Mergeable {
    match word {
        "CLEAN" => Mergeable::Clean,
        "BEHIND" => Mergeable::Behind,
        "BLOCKED" => Mergeable::Blocked,
        "DIRTY" => Mergeable::Conflicting,
        "DRAFT" => Mergeable::Draft,
        "HAS_HOOKS" => Mergeable::HasHooks,
        "UNSTABLE" => Mergeable::Unstable,
        _ => Mergeable::Unknown,
    }
}

fn file_status(word: &str) -> FileStatus {
    match word.to_ascii_uppercase().as_str() {
        "ADDED" => FileStatus::Added,
        "DELETED" | "REMOVED" => FileStatus::Deleted,
        "RENAMED" => FileStatus::Renamed,
        "COPIED" => FileStatus::Copied,
        "CHANGED" => FileStatus::Changed,
        _ => FileStatus::Modified,
    }
}

fn run_status(word: &str) -> RunStatus {
    match word {
        "in_progress" => RunStatus::InProgress,
        "completed" => RunStatus::Completed,
        "waiting" => RunStatus::Waiting,
        "requested" => RunStatus::Requested,
        "pending" => RunStatus::Pending,
        _ => RunStatus::Queued,
    }
}

fn link(value: &Value, kind: Option<ItemKind>) -> LinkRef {
    LinkRef {
        number: count(value, "number"),
        title: text(value, "title"),
        repo: optional(&value["repository"], "nameWithOwner")
            .or_else(|| optional(&value["repository"], "full_name")),
        web_url: optional(value, "url").or_else(|| optional(value, "html_url")),
        kind,
    }
}

fn row(node: &Value, kind: ItemKind) -> ItemRow {
    let rollup = &node["commits"]["nodes"][0]["commit"]["statusCheckRollup"];
    let mut ci_counts: Vec<(CiState, u64)> = Vec::new();
    for counts in [
        &rollup["contexts"]["checkRunCountsByState"],
        &rollup["contexts"]["statusContextCountsByState"],
    ] {
        for entry in counts.as_array().into_iter().flatten() {
            let state = ci_state(entry["state"].as_str().unwrap_or_default());
            let n = count(entry, "count");
            if n == 0 {
                continue;
            }
            match ci_counts.iter_mut().find(|(known, _)| *known == state) {
                Some((_, total)) => *total += n,
                None => ci_counts.push((state, n)),
            }
        }
    }
    ItemRow {
        kind: Some(kind),
        number: count(node, "number"),
        id: id_of(node, "id"),
        title: text(node, "title"),
        author: user(&node["author"]),
        lifecycle: lifecycle(node),
        labels: labels(&node["labels"]),
        updated: timestamp(node, "updatedAt"),
        comments: count(&node["comments"], "totalCount"),
        repo: optional(&node["repository"], "nameWithOwner"),
        web_url: text(node, "url"),
        review: review_decision(node["reviewDecision"].as_str().unwrap_or_default()),
        ci: rollup["state"].as_str().map(ci_state),
        ci_counts,
        additions: node["additions"].as_u64(),
        deletions: node["deletions"].as_u64(),
        changed_files: node["changedFiles"].as_u64(),
        draft: node["isDraft"].as_bool().unwrap_or(false),
        body_preview: node["body"].as_str().map(str::to_owned),
        head_branch: optional(node, "headRefName"),
        review_tally: (kind == ItemKind::PullRequest
            && node.get("latestOpinionatedReviews").is_some())
        .then(|| review_tally(node)),
        created: Some(timestamp(node, "createdAt")).filter(|created| !created.is_empty()),
        linked: node["closedByPullRequestsReferences"]["totalCount"]
            .as_u64()
            .or_else(|| node["closingIssuesReferences"]["totalCount"].as_u64()),
    }
}

/// `latestOpinionatedReviews` (each reviewer's latest approval or change request) and the
/// open `reviewRequests`.
fn review_tally(node: &Value) -> ReviewTally {
    let mut tally = ReviewTally {
        pending: count(&node["reviewRequests"], "totalCount"),
        ..ReviewTally::default()
    };
    for review in nodes(&node["latestOpinionatedReviews"]) {
        match review["state"].as_str().unwrap_or_default() {
            "APPROVED" => tally.approved += 1,
            "CHANGES_REQUESTED" => tally.changes_requested += 1,
            _ => {}
        }
    }
    tally
}

fn check(node: &Value) -> Check {
    if node["__typename"] == "StatusContext" || node.get("context").is_some() {
        let state = text(node, "state");
        return Check {
            name: text(node, "context"),
            workflow: None,
            state: ci_state(&state),
            raw_state: Some(state),
            started: optional_time(node, "createdAt"),
            duration: None,
            completed: None,
            details_url: optional(node, "targetUrl"),
            run_link: None,
            required: node["isRequired"].as_bool().unwrap_or(false),
        };
    }
    let raw = optional(node, "conclusion").unwrap_or_else(|| text(node, "status"));
    let run = &node["checkSuite"]["workflowRun"];
    let details = optional(node, "detailsUrl");
    let job = details
        .as_deref()
        .and_then(|url| url.split("/job/").nth(1))
        .and_then(|suffix| suffix.split(['/', '?']).next())
        .map(ItemId::new);
    Check {
        name: text(node, "name"),
        workflow: optional(&run["workflow"], "name"),
        state: ci_state(&raw),
        raw_state: Some(raw),
        started: optional_time(node, "startedAt"),
        duration: None,
        completed: optional_time(node, "completedAt"),
        details_url: details,
        run_link: run["databaseId"].as_u64().map(|id| RunLink {
            run: ItemId::new(id),
            job,
        }),
        required: node["isRequired"].as_bool().unwrap_or(false),
    }
}

fn review(node: &Value) -> Review {
    Review {
        id: id_of(node, "id"),
        author: user(&node["author"]),
        state: review_state(node["state"].as_str().unwrap_or_default()),
        body: text(node, "body"),
        submitted: timestamp(node, "submittedAt"),
    }
}

fn projects(value: &Value) -> Vec<String> {
    nodes(value)
        .iter()
        .filter_map(|node| optional(&node["project"], "title"))
        .collect()
}

fn subscribed(value: &Value) -> Option<bool> {
    value["viewerSubscription"]
        .as_str()
        .map(|state| state == "SUBSCRIBED")
}

fn issue(node: &Value) -> Issue {
    Issue {
        row: row(node, ItemKind::Issue),
        body: text(node, "body"),
        assignees: nodes(&node["assignees"]).iter().map(user).collect(),
        milestone: milestone(&node["milestone"]),
        linked_prs: nodes(&node["closedByPullRequestsReferences"])
            .iter()
            .map(|node| link(node, Some(ItemKind::PullRequest)))
            .collect(),
        created: timestamp(node, "createdAt"),
        closed_reason: match node["stateReason"].as_str() {
            Some("COMPLETED") => Some(ClosedReason::Completed),
            Some("NOT_PLANNED") => Some(ClosedReason::NotPlanned),
            Some("DUPLICATE") => Some(ClosedReason::Duplicate),
            _ => None,
        },
        projects: projects(&node["projectItems"]),
        subscribed: subscribed(node),
        reactions: reactions(node),
    }
}

fn changed_file(node: &Value) -> ChangedFile {
    ChangedFile {
        path: text(node, "path"),
        old_path: None,
        status: file_status(node["changeType"].as_str().unwrap_or_default()),
        additions: count(node, "additions"),
        deletions: count(node, "deletions"),
        viewed: node["viewerViewedState"]
            .as_str()
            .map(|state| state == "VIEWED"),
    }
}

fn pull_request(node: &Value, files: Vec<ChangedFile>) -> PullRequest {
    let rollup = &node["latest"]["nodes"][0]["commit"]["statusCheckRollup"];
    // A rerun leaves its earlier attempt in the rollup; one check per (workflow, name) and the
    // state computed over those, so a superseded failure no longer reads as failing.
    let checks = checks::dedupe(
        nodes(&rollup["contexts"])
            .iter()
            .map(check)
            .collect::<Vec<_>>(),
    );
    let head = text(node, "headRefOid");
    let mut row = row(node, ItemKind::PullRequest);
    row.ci = checks::rollup(&checks).or_else(|| rollup["state"].as_str().map(ci_state));
    let reviewers = nodes(&node["reviewRequests"])
        .iter()
        .filter_map(|request| {
            let reviewer = &request["requestedReviewer"];
            optional(reviewer, "login").or_else(|| optional(reviewer, "slug"))
        })
        .map(|reviewer| ReviewerSlot {
            reviewer,
            state: ReviewState::Requested,
        })
        .collect();
    PullRequest {
        body: text(node, "body"),
        base: RefInfo {
            repo: None,
            branch: text(node, "baseRefName"),
            sha: text(node, "baseRefOid"),
        },
        head: RefInfo {
            repo: optional(&node["headRepository"], "nameWithOwner"),
            branch: text(node, "headRefName"),
            sha: head.clone(),
        },
        mergeable: mergeable(node["mergeStateStatus"].as_str().unwrap_or_default()),
        reviewers,
        reviews: nodes(&node["reviews"]).iter().map(review).collect(),
        checks,
        files,
        commits: nodes(&node["commits"])
            .iter()
            .map(|node| {
                let commit = &node["commit"];
                CommitSummary {
                    sha: text(commit, "oid"),
                    headline: text(commit, "messageHeadline"),
                    body: text(commit, "messageBody"),
                    committed: timestamp(commit, "committedDate"),
                    author: optional(&commit["author"]["user"], "login").map(User::login),
                }
            })
            .collect(),
        assignees: nodes(&node["assignees"]).iter().map(user).collect(),
        milestone: milestone(&node["milestone"]),
        linked_issues: nodes(&node["closingIssuesReferences"])
            .iter()
            .map(|node| link(node, Some(ItemKind::Issue)))
            .collect(),
        created: timestamp(node, "createdAt"),
        pinned_head: head.clone(),
        fingerprint: Fingerprint {
            state: text(node, "state"),
            mergeable: text(node, "mergeable"),
            head,
            rollup: row.ci,
            updated: timestamp(node, "updatedAt"),
        },
        projects: projects(&node["projectItems"]),
        subscribed: subscribed(node),
        reactions: reactions(node),
        row,
    }
}

fn comment(node: &Value) -> Comment {
    let position = optional(node, "path").map(|path| CommentPosition {
        path,
        line: node["line"].as_u64(),
        original_line: node["originalLine"].as_u64(),
        commit: optional(&node["commit"], "oid"),
    });
    Comment {
        id: id_of(node, "id"),
        author: user(&node["author"]),
        body: text(node, "body"),
        created: timestamp(node, "createdAt"),
        system: false,
        pending: false,
        position,
        association: optional(node, "authorAssociation"),
        edited: optional_time(node, "lastEditedAt"),
        reactions: reactions(node),
    }
}

fn thread(node: &Value) -> ReviewThread {
    let comments = nodes(&node["comments"])
        .iter()
        .map(comment)
        .collect::<Vec<_>>();
    let total = node["comments"]["totalCount"].as_u64();
    ReviewThread {
        id: id_of(node, "id"),
        resolved: node["isResolved"].as_bool(),
        resolvable: true,
        path: optional(node, "path"),
        line: node["line"].as_u64(),
        original_line: node["originalLine"].as_u64(),
        side: if node["diffSide"] == "LEFT" {
            Side::Left
        } else {
            Side::Right
        },
        commit: comments
            .first()
            .and_then(|comment| comment.position.as_ref()?.commit.clone()),
        total_comments: total.filter(|total| *total > comments.len() as u64),
        diff_hunk: nodes(&node["comments"])
            .first()
            .and_then(|first| optional(first, "diffHunk")),
        start_line: node["startLine"].as_u64(),
        comments,
    }
}

fn actor(event: &Value) -> Actor {
    let who = ["actor", "user", "author"]
        .into_iter()
        .map(|key| &event[key])
        .find(|value| value["login"].is_string())
        .map(user)
        .unwrap_or_default();
    let at = ["created_at", "submitted_at"]
        .into_iter()
        .find_map(|key| optional(event, key))
        .or_else(|| optional(&event["committer"], "date"))
        .unwrap_or_default();
    Actor {
        actor: who,
        at: Timestamp::new(at),
    }
}

fn timeline_event(event: &Value) -> TimelineEvent {
    let by = actor(event);
    let kind = text(event, "event");
    match kind.as_str() {
        "commented" => TimelineEvent::Commented(Comment {
            id: id_of(event, "node_id"),
            author: by.actor,
            body: text(event, "body"),
            created: by.at,
            system: false,
            pending: false,
            position: None,
            association: optional(event, "author_association"),
            edited: edited(event, "created_at", "updated_at"),
            reactions: reactions(event),
        }),
        "reviewed" => TimelineEvent::Reviewed(Review {
            id: id_of(event, "node_id"),
            author: by.actor,
            state: review_state(event["state"].as_str().unwrap_or_default()),
            body: text(event, "body"),
            submitted: by.at,
        }),
        "labeled" => TimelineEvent::Labeled {
            label: text(&event["label"], "name"),
            by,
        },
        "unlabeled" => TimelineEvent::Unlabeled {
            label: text(&event["label"], "name"),
            by,
        },
        "assigned" => TimelineEvent::Assigned {
            assignee: text(&event["assignee"], "login"),
            by,
        },
        "unassigned" => TimelineEvent::Unassigned {
            assignee: text(&event["assignee"], "login"),
            by,
        },
        // A closing commit or reason words the event ("closed this as not planned").
        "closed" => match detail(event) {
            detail if detail.is_empty() => TimelineEvent::Closed { by },
            detail => TimelineEvent::Detailed { kind, detail, by },
        },
        "reopened" => TimelineEvent::Reopened { by },
        "merged" => TimelineEvent::Merged {
            sha: text(event, "commit_id"),
            by,
        },
        "committed" => TimelineEvent::Committed(CommitSummary {
            sha: text(event, "sha"),
            headline: text(event, "message")
                .lines()
                .next()
                .unwrap_or_default()
                .into(),
            body: text(event, "message"),
            committed: by.at,
            // REST names a commit's author by name and email, not login.
            author: Some(by.actor)
                .filter(|user| !user.login.is_empty())
                .or_else(|| {
                    optional(&event["author"], "name").map(|name| User {
                        name: Some(name),
                        ..User::default()
                    })
                }),
        }),
        // REST sends the new head as `commit_id`; `before`/`after` are kept for older caches.
        "head_ref_force_pushed" => TimelineEvent::ForcePushed {
            from: text(event, "before"),
            to: optional(event, "after")
                .or_else(|| optional(event, "commit_id"))
                .unwrap_or_default(),
            by,
        },
        "milestoned" => TimelineEvent::Milestoned {
            milestone: text(&event["milestone"], "title"),
            by,
        },
        "demilestoned" => TimelineEvent::Demilestoned {
            milestone: text(&event["milestone"], "title"),
            by,
        },
        "cross-referenced" => {
            let issue = &event["source"]["issue"];
            TimelineEvent::CrossReferenced {
                link: link(
                    issue,
                    Some(if issue.get("pull_request").is_some() {
                        ItemKind::PullRequest
                    } else {
                        ItemKind::Issue
                    }),
                ),
                by,
            }
        }
        "renamed" => TimelineEvent::Renamed {
            from: text(&event["rename"], "from"),
            to: text(&event["rename"], "to"),
            by,
        },
        "review_requested" => TimelineEvent::ReviewRequested {
            reviewer: optional(&event["requested_reviewer"], "login")
                .or_else(|| optional(&event["requested_team"], "slug"))
                .unwrap_or_default(),
            by,
        },
        _ => match detail(event) {
            detail if detail.is_empty() => TimelineEvent::Other { kind, by },
            detail => TimelineEvent::Detailed { kind, detail, by },
        },
    }
}

/// The REST fields GitHub's timeline words an event from (`TimelineEvent::Detailed`).
fn detail(event: &Value) -> std::collections::BTreeMap<String, String> {
    const PATHS: [&str; 9] = [
        "state_reason",
        "commit_id",
        "lock_reason",
        "requested_reviewer.login",
        "requested_team.name",
        "project_card.column_name",
        "deployment.environment",
        "previous_ref_name",
        "current_ref_name",
    ];
    PATHS
        .iter()
        .filter_map(|path| {
            let value = path.split('.').fold(event, |value, key| &value[key]);
            value
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| ((*path).to_owned(), text.to_owned()))
        })
        .collect()
}

fn workflow(value: &Value) -> Workflow {
    Workflow {
        id: id_of(value, "id"),
        name: text(value, "name"),
        path: text(value, "path"),
        state: text(value, "state"),
        dispatchable: None,
    }
}

fn run(value: &Value) -> Run {
    let conclusion = optional(value, "conclusion");
    Run {
        id: id_of(value, "id"),
        number: count(value, "run_number"),
        title: optional(value, "display_title").unwrap_or_else(|| text(value, "name")),
        workflow: value["workflow_id"].as_u64().map(|id| Workflow {
            id: ItemId::new(id),
            name: text(value, "name"),
            path: text(value, "path"),
            state: String::new(),
            dispatchable: None,
        }),
        status: run_status(value["status"].as_str().unwrap_or_default()),
        conclusion: conclusion.as_deref().map(ci_state),
        conclusion_raw: conclusion,
        branch: text(value, "head_branch"),
        sha: text(value, "head_sha"),
        event: text(value, "event"),
        // A re-run starts long after its run was created: `running 3m`, not `running 2d`.
        created: optional_time(value, "run_started_at")
            .or_else(|| optional_time(value, "created_at"))
            .unwrap_or_default(),
        duration: (value["status"] == "completed")
            .then(|| span(value, "run_started_at", "updated_at"))
            .flatten(),
        attempt: value["run_attempt"].as_u64().unwrap_or(1) as u32,
        attempts: value["run_attempt"].as_u64().unwrap_or(1) as u32,
        web_url: text(value, "html_url"),
        pr_number: value["pull_requests"][0]["number"].as_u64(),
        needs_approval: value["conclusion"] == "action_required",
        actor: run_actor(value),
    }
}

fn step(value: &Value) -> Step {
    let conclusion = optional(value, "conclusion");
    Step {
        number: count(value, "number"),
        name: text(value, "name"),
        status: run_status(value["status"].as_str().unwrap_or_default()),
        conclusion: conclusion.as_deref().map(ci_state),
        conclusion_raw: conclusion,
        started: optional_time(value, "started_at"),
        completed: optional_time(value, "completed_at"),
    }
}

/// `steps[]` when the jobs API lists them; a job that started without them is cut by its
/// log's `##[group]` sections; one that never started has none.
fn job_steps(value: &Value) -> Steps {
    let steps = value["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(step)
        .collect::<Vec<_>>();
    if !steps.is_empty() {
        Steps::Known(steps)
    } else if value["started_at"].is_null() || value["conclusion"] == "skipped" {
        Steps::Unavailable("This job never ran".into())
    } else {
        Steps::FromLogSections
    }
}

/// The run's head as the approval check sees it.
fn run_identity(value: &Value) -> RunIdentity {
    RunIdentity {
        sha: text(value, "head_sha"),
        branch: text(value, "head_branch"),
        head_owner: text(&value["head_repository"]["owner"], "login"),
    }
}

/// `query Name(...) { repository(...) { pullRequest(number: $number) { BODY } } }` as one
/// query with an alias `p<N>` per number (batched).
pub fn alias_document(document: &str, numbers: &[u64]) -> Option<String> {
    let head = "pullRequest(number: $number) {";
    let start = document.find(head)? + head.len();
    let name_start = document.find("query ")? + "query ".len();
    let name_end = name_start + document[name_start..].find('(')?;
    let name = &document[name_start..name_end];
    let mut body = document[start..].trim_end();
    for _ in 0..3 {
        body = body.strip_suffix('}')?.trim_end();
    }
    let aliases = numbers
        .iter()
        .map(|number| format!("p{number}: pullRequest(number: {number}) {{ {body} }}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "query {name}Batch($owner: String!, $name: String!) {{ \
repository(owner: $owner, name: $name) {{ {aliases} }} }}"
    ))
}

fn fingerprint(node: &Value) -> Fingerprint {
    Fingerprint {
        state: text(node, "state"),
        mergeable: text(node, "mergeable"),
        head: text(node, "headRefOid"),
        rollup: node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"]
            .as_str()
            .map(ci_state),
        updated: timestamp(node, "updatedAt"),
    }
}

fn job(value: &Value) -> Job {
    let conclusion = optional(value, "conclusion");
    Job {
        id: id_of(value, "id"),
        name: text(value, "name"),
        status: run_status(value["status"].as_str().unwrap_or_default()),
        conclusion: conclusion.as_deref().map(ci_state),
        conclusion_raw: conclusion,
        started: optional_time(value, "started_at"),
        completed: optional_time(value, "completed_at"),
        steps: job_steps(value),
        web_url: text(value, "html_url"),
        stage: None,
        retried: value["run_attempt"]
            .as_u64()
            .is_some_and(|attempt| attempt > 1),
    }
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
        (Some("PullRequest"), Some(number)) => NotifyTarget::Item {
            kind: ItemKind::PullRequest,
            number,
        },
        (Some("Issue"), Some(number)) => NotifyTarget::Item {
            kind: ItemKind::Issue,
            number,
        },
        (Some("CheckSuite"), _) => NotifyTarget::Run(ItemId::new(&url)),
        (Some("Release"), _) => NotifyTarget::Release(url.clone()),
        (kind, _) => NotifyTarget::Other(kind.unwrap_or_default().into()),
    };
    Notification {
        id: id_of(value, "id"),
        title: text(subject, "title"),
        reason: match value["reason"].as_str().unwrap_or_default() {
            "mention" => NotifyReason::Mentioned,
            // GitHub's own "Team mentioned" filter keeps it apart from a mention of you.
            "team_mention" => NotifyReason::Other("team_mention".into()),
            "review_requested" => NotifyReason::ReviewRequested,
            "assign" => NotifyReason::Assigned,
            "author" => NotifyReason::Author,
            "comment" => NotifyReason::Comment,
            "state_change" => NotifyReason::StateChange,
            "ci_activity" => NotifyReason::CiActivity,
            "subscribed" | "manual" => NotifyReason::Subscribed,
            other => NotifyReason::Other(other.into()),
        },
        unread: value["unread"].as_bool().unwrap_or(false),
        repo: text(&value["repository"], "full_name"),
        target,
        updated: timestamp(value, "updated_at"),
        comment: optional(subject, "latest_comment_url")
            .and_then(|url| url.rsplit('/').next().map(ItemId::new)),
    }
}

fn repo_payload(value: &Value) -> Repo {
    let permissions = &value["permissions"];
    let mut merge_methods = Vec::new();
    for (key, method) in [
        ("allow_merge_commit", MergeMethod::Merge),
        ("allow_squash_merge", MergeMethod::Squash),
        ("allow_rebase_merge", MergeMethod::Rebase),
    ] {
        // Only reported to users with push access; absent means "allowed".
        if value[key].as_bool() != Some(false) {
            merge_methods.push(method);
        }
    }
    Repo {
        id: id_of(value, "node_id"),
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
        default_merge_method: None,
        delete_branch_on_merge: value["delete_branch_on_merge"].as_bool().unwrap_or(false),
        has_ci: true,
        pr_template: None,
    }
}

/// The detail documents' Projects (v2) field. GitHub checks scopes before running a query, so a
/// token without `read:project` (gh's default login) fails the whole detail on it.
const PROJECT_ITEMS: &str = "projectItems(first: 20) { nodes { project { title } } } ";

fn needs_project_scope(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Forbidden(message) if message.contains("read:project"))
}

/// `document` without Projects, asked again when the token lacks `read:project` (as gh does);
/// the item then shows no projects.
fn without_projects(document: &str) -> String {
    document.replace(PROJECT_ITEMS, "")
}

/// GraphQL's 200-with-errors: rate limits and missing data become errors; partial data
/// (a `projectItems` scope error, say) is kept.
fn graphql_data(body: Value) -> Result<Value, ForgeError> {
    let errors = body["errors"].as_array().cloned().unwrap_or_default();
    let first = |key: &str| {
        errors
            .iter()
            .find_map(|error| error[key].as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    if errors
        .iter()
        .any(|error| error["type"] == "RATE_LIMITED" || error["code"] == "RATE_LIMITED")
    {
        return Err(ForgeError::RateLimited {
            until: std::time::Instant::now() + std::time::Duration::from_secs(60),
        });
    }
    let data = body["data"].clone();
    if data.is_null()
        || (data
            .as_object()
            .is_some_and(|object| object.values().all(Value::is_null))
            && !errors.is_empty())
    {
        let message = crate::cli::redact(&first("message"));
        return Err(match first("type").as_str() {
            "NOT_FOUND" => ForgeError::NotFound(message),
            "INSUFFICIENT_SCOPES" => ForgeError::Forbidden(
                crate::http::scopes_in(&message)
                    .map_or(message, |scopes| format!("needs scope {scopes}")),
            ),
            "FORBIDDEN" => ForgeError::Forbidden(message),
            _ if errors.is_empty() => ForgeError::BadResponse("GraphQL returned no data".into()),
            _ => ForgeError::Failed(message),
        });
    }
    Ok(data)
}

impl GitHub {
    fn graphql(
        ctx: &ForgeContext,
        document: &str,
        variables: Value,
        cancel: &AtomicBool,
    ) -> Result<Value, ForgeError> {
        let send = |document: &str, variables: Value| {
            let response = ctx.send(&ApiRequest::graphql(document, variables), cancel)?;
            if !response.success() {
                return Err(crate::http::classify(&ctx.endpoints, &response));
            }
            graphql_data(response.json()?)
        };
        match send(document, variables.clone()) {
            Err(error) if needs_project_scope(&error) && document.contains(PROJECT_ITEMS) => {
                send(&without_projects(document), variables)
            }
            result => result,
        }
    }

    fn repo_variables(ctx: &ForgeContext, number: u64) -> Result<Value, ForgeError> {
        let repo = ctx.repo()?;
        Ok(json!({ "owner": repo.owner, "name": repo.name, "number": number }))
    }

    /// The search string the list and inbox send: GitHub search syntax, user terms kept.
    pub fn search_query(
        repo: Option<&RepoRef>,
        kind: ItemKind,
        filter: &Filter,
        sort: &str,
    ) -> String {
        let mut terms = Vec::new();
        if let Some(repo) = repo {
            terms.push(format!("repo:{}", repo.full()));
        }
        terms.push(
            match kind {
                ItemKind::Issue => "is:issue",
                ItemKind::PullRequest => "is:pr",
            }
            .to_owned(),
        );
        match filter.state {
            StateFilter::Open => terms.push("is:open".into()),
            StateFilter::Closed if kind == ItemKind::PullRequest => {
                terms.push("is:closed".into());
                terms.push("is:unmerged".into());
            }
            StateFilter::Closed => terms.push("is:closed".into()),
            StateFilter::Merged => terms.push("is:merged".into()),
            StateFilter::All => {}
        }
        let query = filter.query.trim();
        if !query.is_empty() {
            terms.push(query.to_owned());
        }
        if !query
            .split_whitespace()
            .any(|term| term.starts_with("sort:"))
        {
            terms.push(format!("sort:{sort}"));
        }
        terms.join(" ")
    }

    fn rows(
        ctx: &ForgeContext,
        query: String,
        other: Option<String>,
        kind: ItemKind,
        page: Page,
        cancel: &AtomicBool,
    ) -> Result<PageOf<ItemRow>, ForgeError> {
        let document = list_document(kind, other.is_some());
        let mut other_total = None;
        let skip = (page.number.max(1) as usize - 1) * page.per_page as usize;
        let wanted = skip + page.per_page.max(1) as usize;
        let mut after: Option<String> = None;
        let mut items = Vec::new();
        let mut total = None;
        let mut more = false;
        while items.len() < wanted {
            if cancel.load(Ordering::Acquire) {
                return Err(ForgeError::Cancelled);
            }
            let first = (wanted - items.len()).min(PAGE as usize);
            let mut variables = json!({ "q": query, "first": first, "after": after });
            if let Some(other) = &other {
                variables["other"] = other.clone().into();
            }
            let data = Self::graphql(ctx, &document, variables, cancel)?;
            let search = &data["search"];
            total = search["issueCount"].as_u64();
            other_total = other_total.or(data["other"]["issueCount"].as_u64());
            items.extend(
                nodes(search)
                    .iter()
                    .filter(|node| node.get("number").is_some())
                    .map(|node| row(node, kind)),
            );
            more = search["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            after = search["pageInfo"]["endCursor"].as_str().map(str::to_owned);
            if !more || after.is_none() || nodes(search).is_empty() {
                break;
            }
        }
        let items = items
            .into_iter()
            .skip(skip)
            .take(page.per_page as usize)
            .collect::<Vec<_>>();
        Ok(PageOf {
            next: more.then_some(Page {
                number: page.number + 1,
                per_page: page.per_page,
            }),
            items,
            total,
            other_total,
        })
    }

    /// GraphQL connection pages under `repository.pullRequest.<field>`.
    fn connection(
        ctx: &ForgeContext,
        document: &str,
        number: u64,
        field: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, ForgeError> {
        let mut variables = Self::repo_variables(ctx, number)?;
        let mut all = Vec::new();
        for _ in 0..MAX_PAGES {
            if cancel.load(Ordering::Acquire) {
                return Err(ForgeError::Cancelled);
            }
            let data = Self::graphql(ctx, document, variables.clone(), cancel)?;
            let pull = &data["repository"]["pullRequest"];
            if pull.is_null() {
                return Err(ForgeError::NotFound(format!("pull request #{number}")));
            }
            let connection = &pull[field];
            all.extend(nodes(connection).iter().cloned());
            let info = &connection["pageInfo"];
            match info["endCursor"].as_str() {
                Some(cursor) if info["hasNextPage"].as_bool() == Some(true) => {
                    variables["after"] = cursor.into();
                }
                _ => break,
            }
        }
        Ok(all)
    }

    /// REST pages following `Link: rel="next"`.
    fn rest_pages(
        ctx: &ForgeContext,
        first: ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, ForgeError> {
        link_pages(ctx, first, MAX_PAGES, cancel)
    }

    fn text_read(
        ctx: &ForgeContext,
        request: ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<String, ForgeError> {
        Ok(ctx.ok(&request, cancel)?.text())
    }

    fn requests(ctx: &ForgeContext, read: &Read) -> Result<Vec<ApiRequest>, ForgeError> {
        let rest = |tail: &str| repos_path(ctx, tail).map(ApiRequest::get);
        let graphql = |document: &str, number: u64| {
            Self::repo_variables(ctx, number)
                .map(|variables| ApiRequest::graphql(document, variables))
        };
        Ok(match read {
            Read::Repository => vec![rest("")?],
            Read::RepositoryNamed(name) => vec![ApiRequest::get(format!("repos/{name}"))],
            Read::Viewer => vec![ApiRequest::get("user")],
            Read::Labels => vec![rest("labels")?.query("per_page", PAGE)],
            Read::Milestones => vec![
                rest("milestones")?
                    .query("state", "all")
                    .query("per_page", PAGE),
            ],
            Read::Collaborators => vec![rest("assignees")?.query("per_page", PAGE)],
            Read::List { kind, filter, page } => {
                let other = other_state(filter);
                let mut variables = json!({
                    "q": Self::search_query(ctx.repo.as_ref(), *kind, filter, "created-desc"),
                    "first": page.per_page.min(PAGE),
                    "after": Value::Null,
                });
                if let Some(other) = &other {
                    variables["other"] =
                        Self::search_query(ctx.repo.as_ref(), *kind, other, "created-desc").into();
                }
                vec![ApiRequest::graphql(
                    &list_document(*kind, other.is_some()),
                    variables,
                )]
            }
            Read::Search { kind, filter, page } => vec![ApiRequest::graphql(
                &list_document(*kind, false),
                json!({
                    "q": Self::search_query(None, *kind, filter, "updated-desc"),
                    "first": page.per_page.min(PAGE),
                    "after": Value::Null,
                }),
            )],
            Read::PullRequest(number) => vec![graphql(PULL_REQUEST_DOCUMENT, *number)?],
            Read::Issue(number) => vec![graphql(ISSUE_DOCUMENT, *number)?],
            Read::Fingerprint(number) => vec![graphql(FINGERPRINT_DOCUMENT, *number)?],
            Read::Timeline(number) => {
                vec![rest(&format!("issues/{number}/timeline"))?.query("per_page", PAGE)]
            }
            Read::Threads(number) => vec![graphql(THREADS_DOCUMENT, *number)?],
            Read::Viewed(number) => vec![graphql(FILES_DOCUMENT, *number)?],
            Read::Diff(number) => vec![rest(&format!("pulls/{number}"))?.accept(Accept::Diff)],
            Read::CommitDiff(sha) => vec![rest(&format!("commits/{sha}"))?.accept(Accept::Diff)],
            Read::PullRequestForBranch(branch) => {
                let owner = ctx.repo()?.owner.clone();
                vec![
                    rest("pulls")?
                        .query("head", format!("{owner}:{branch}"))
                        .query("state", "open")
                        .query("per_page", 1),
                ]
            }
            Read::Workflows => vec![rest("actions/workflows")?.query("per_page", PAGE)],
            Read::WorkflowSource { id, .. } => vec![rest(&format!("actions/workflows/{id}"))?],
            Read::Runs { filter, page } => {
                let mut request = match &filter.workflow {
                    Some(id) => rest(&format!("actions/workflows/{id}/runs"))?,
                    None => rest("actions/runs")?,
                };
                if let Some(branch) = &filter.branch {
                    request = request.query("branch", branch);
                }
                if let Some(event) = &filter.event {
                    request = request.query("event", event);
                }
                if let Some(status) = &filter.status {
                    request = request.query("status", status);
                }
                request = request.query("per_page", page.per_page.min(PAGE));
                if page.number > 1 {
                    request = request.query("page", page.number);
                }
                vec![request]
            }
            Read::Run { id, attempt } => match attempt {
                Some(attempt) => vec![
                    rest(&format!("actions/runs/{id}/attempts/{attempt}"))?,
                    rest(&format!("actions/runs/{id}/attempts/{attempt}/jobs"))?
                        .query("per_page", PAGE),
                ],
                None => vec![
                    rest(&format!("actions/runs/{id}"))?,
                    rest(&format!("actions/runs/{id}/jobs"))?
                        .query("filter", "latest")
                        .query("per_page", PAGE),
                ],
            },
            Read::Job(id) => vec![rest(&format!("actions/jobs/{id}"))?],
            Read::JobLog(id) => vec![
                rest(&format!("actions/jobs/{id}/logs"))?
                    .accept(Accept::Text)
                    .timeout(crate::transport::LOG_TIMEOUT),
            ],
            Read::Artifacts(run) => {
                vec![rest(&format!("actions/runs/{run}/artifacts"))?.query("per_page", PAGE)]
            }
            Read::Notifications { filter, page } => {
                let mut request = ApiRequest::get("notifications")
                    .query("all", filter.all)
                    .query("per_page", page.per_page.min(50))
                    .class(crate::transport::Class::Background);
                if let Some(since) = &filter.since {
                    request = request.query("since", since.as_str());
                }
                if page.number > 1 {
                    request = request.query("page", page.number);
                }
                vec![request]
            }
            Read::PullRequestTemplate => Vec::new(),
            Read::PullRequests(numbers) | Read::Fingerprints(numbers) => {
                let repo = ctx.repo()?;
                let document = match read {
                    Read::PullRequests(_) => PULL_REQUEST_DOCUMENT,
                    _ => FINGERPRINT_DOCUMENT,
                };
                let limit = crate::batch::limits(read.class()).aliases;
                numbers
                    .chunks(limit)
                    .filter_map(|chunk| alias_document(document, chunk))
                    .map(|text| {
                        ApiRequest::graphql(
                            &text,
                            json!({ "owner": repo.owner, "name": repo.name }),
                        )
                    })
                    .collect()
            }
            Read::ArtifactFiles(id) | Read::ArtifactFile { artifact: id, .. } => {
                vec![super::download(rest(&format!(
                    "actions/artifacts/{id}/zip"
                ))?)]
            }
        })
    }

    /// Per-number answers of an aliased batch; one number sends the plain document.
    fn batched(
        ctx: &ForgeContext,
        document: &'static str,
        numbers: &[u64],
        class: crate::transport::Class,
        cancel: &AtomicBool,
    ) -> Result<crate::batch::Answers, ForgeError> {
        let repo = ctx.repo()?;
        let operation = document
            .split_whitespace()
            .nth(1)
            .and_then(|name| name.split('(').next())
            .unwrap_or_default();
        let group = format!("{}|{}|{operation}", ctx.endpoints.api_base, repo.full());
        let send = |chunk: &[u64]| -> Result<crate::batch::Answers, ForgeError> {
            let (mut request, single) = match chunk {
                [number] => (
                    ApiRequest::graphql(document, Self::repo_variables(ctx, *number)?),
                    true,
                ),
                _ => (
                    ApiRequest::graphql(
                        &alias_document(document, chunk).ok_or_else(|| {
                            ForgeError::BadResponse("document cannot be batched".into())
                        })?,
                        json!({ "owner": repo.owner, "name": repo.name }),
                    ),
                    false,
                ),
            };
            request.class = class;
            let response = ctx.send(&request, cancel)?;
            if !response.success() {
                return Err(crate::http::classify(&ctx.endpoints, &response));
            }
            let data = match graphql_data(response.json()?) {
                Err(ForgeError::NotFound(_)) => return Ok(Default::default()),
                data => data?,
            };
            Ok(chunk
                .iter()
                .map(|number| {
                    let node = if single {
                        data["repository"]["pullRequest"].clone()
                    } else {
                        data["repository"][format!("p{number}")].clone()
                    };
                    (*number, node)
                })
                .collect())
        };
        crate::batch::submit(&group, class, numbers, cancel, &send)
    }

    fn pull_requests(
        ctx: &ForgeContext,
        numbers: &[u64],
        cancel: &AtomicBool,
    ) -> Result<Vec<PullRequest>, ForgeError> {
        let answers = Self::batched(
            ctx,
            PULL_REQUEST_DOCUMENT,
            numbers,
            crate::transport::Class::Interactive,
            cancel,
        )?;
        let mut pulls = Vec::new();
        for number in numbers {
            let Some(node) = answers.get(number).filter(|node| node.is_object()) else {
                continue;
            };
            let mut files = nodes(&node["files"])
                .iter()
                .map(changed_file)
                .collect::<Vec<_>>();
            if node["files"]["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
                files = Self::connection(ctx, FILES_DOCUMENT, *number, "files", cancel)?
                    .iter()
                    .map(changed_file)
                    .collect();
            }
            pulls.push(pull_request(node, files));
        }
        Ok(pulls)
    }

    /// Fork-run approval (t3code): the run must still be waiting and its head must be the
    /// one the user looked at (sha, branch, fork owner) before `approve` is sent.
    fn approve(
        ctx: &ForgeContext,
        run: &ItemId,
        expected: &RunIdentity,
        cancel: &AtomicBool,
    ) -> Result<(), ForgeError> {
        let value = ctx.json(
            &ApiRequest::get(repos_path(ctx, &format!("actions/runs/{run}"))?),
            cancel,
        )?;
        if value["conclusion"] != "action_required" {
            return Err(ForgeError::Conflict(
                "this run is not waiting for approval".into(),
            ));
        }
        let found = run_identity(&value);
        let mut changed = Vec::new();
        if !found.sha.eq_ignore_ascii_case(&expected.sha) {
            changed.push("head commit");
        }
        if found.branch != expected.branch {
            changed.push("branch");
        }
        if !expected.head_owner.is_empty()
            && !found.head_owner.eq_ignore_ascii_case(&expected.head_owner)
        {
            changed.push("fork owner");
        }
        if !changed.is_empty() {
            return Err(ForgeError::Conflict(format!(
                "the run's {} changed since you looked · reload before approving",
                changed.join(" and ")
            )));
        }
        Self::send_write(
            ctx,
            ApiRequest::new(
                Method::Post,
                repos_path(ctx, &format!("actions/runs/{run}/approve"))?,
            ),
            cancel,
        )?;
        Ok(())
    }

    fn fetch(
        ctx: &ForgeContext,
        read: &Read,
        cond: &Conditional,
        cancel: &AtomicBool,
    ) -> Result<Fetched, ForgeError> {
        if ctx.anonymous()
            && let Some(result) = anonymous::fetch(ctx, read, cond, cancel)
        {
            return result;
        }
        let mut requests = Self::requests(ctx, read)?;
        let class = read.class();
        for request in &mut requests {
            request.class = class;
        }
        // Incremental refresh: only notifications updated since the cached copy.
        if let (Read::Notifications { filter, .. }, Some(since)) = (read, &cond.since)
            && filter.since.is_none()
            && let Some(request) = requests.first_mut()
        {
            request.query.push(("since".into(), since.as_str().into()));
        }
        // Single REST reads revalidate with ETags; a 304 costs no rate limit.
        if let ([request], Some(etag)) = (requests.as_mut_slice(), &cond.etag)
            && matches!(request.target, Target::Rest(_))
            && request.method == Method::Get
        {
            request.headers.push(("If-None-Match".into(), etag.clone()));
        }
        let payload = match read {
            Read::Repository | Read::RepositoryNamed(_) => {
                let response = ctx.send(&requests[0], cancel)?;
                if response.status == 304 {
                    return Ok(Fetched::NotModified);
                }
                if !response.success() {
                    return Err(crate::http::classify(&ctx.endpoints, &response));
                }
                let etag = response.header("etag").map(str::to_owned);
                return Ok(Fetched::Payload {
                    value: Payload::Repo(repo_payload(&response.json()?)),
                    etag,
                    fingerprint: None,
                });
            }
            Read::Viewer => Payload::User(user(&ctx.json(&requests[0], cancel)?)),
            Read::Labels => Payload::Labels(labels(&Value::Array(Self::rest_pages(
                ctx,
                requests.remove(0),
                cancel,
            )?))),
            Read::Milestones => Payload::Milestones(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .iter()
                    .filter_map(milestone)
                    .collect(),
            ),
            Read::Collaborators => Payload::Users(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .iter()
                    .map(user)
                    .collect(),
            ),
            Read::List { kind, filter, page } => Payload::Rows(Self::rows(
                ctx,
                Self::search_query(ctx.repo.as_ref(), *kind, filter, "created-desc"),
                other_state(filter).map(|other| {
                    Self::search_query(ctx.repo.as_ref(), *kind, &other, "created-desc")
                }),
                *kind,
                *page,
                cancel,
            )?),
            Read::Search { kind, filter, page } => Payload::Rows(Self::rows(
                ctx,
                Self::search_query(None, *kind, filter, "updated-desc"),
                None,
                *kind,
                *page,
                cancel,
            )?),
            Read::Issue(number) => {
                let data = Self::graphql(
                    ctx,
                    ISSUE_DOCUMENT,
                    Self::repo_variables(ctx, *number)?,
                    cancel,
                )?;
                let node = &data["repository"]["issue"];
                if !node.is_object() {
                    return Err(ForgeError::NotFound(format!("issue #{number}")));
                }
                Payload::Issue(Box::new(issue(node)))
            }
            Read::PullRequest(number) => {
                let data = Self::graphql(
                    ctx,
                    PULL_REQUEST_DOCUMENT,
                    Self::repo_variables(ctx, *number)?,
                    cancel,
                )?;
                let node = &data["repository"]["pullRequest"];
                if !node.is_object() {
                    return Err(ForgeError::NotFound(format!("pull request #{number}")));
                }
                let mut files = nodes(&node["files"])
                    .iter()
                    .map(changed_file)
                    .collect::<Vec<_>>();
                if node["files"]["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
                    files = Self::connection(ctx, FILES_DOCUMENT, *number, "files", cancel)?
                        .iter()
                        .map(changed_file)
                        .collect();
                }
                let pull = pull_request(node, files);
                let fingerprint = Some(pull.fingerprint.clone());
                return Ok(Fetched::Payload {
                    value: Payload::PullRequest(Box::new(pull)),
                    etag: None,
                    fingerprint,
                });
            }
            Read::Fingerprint(number) => {
                let data = Self::graphql(
                    ctx,
                    FINGERPRINT_DOCUMENT,
                    Self::repo_variables(ctx, *number)?,
                    cancel,
                )?;
                let node = &data["repository"]["pullRequest"];
                if !node.is_object() {
                    return Err(ForgeError::NotFound(format!("pull request #{number}")));
                }
                let fingerprint = fingerprint(node);
                if cond.fingerprint.as_ref() == Some(&fingerprint) {
                    return Ok(Fetched::NotModified);
                }
                Payload::Fingerprint(fingerprint)
            }
            Read::PullRequests(numbers) => {
                Payload::PullRequests(Self::pull_requests(ctx, numbers, cancel)?)
            }
            Read::Fingerprints(numbers) => {
                let answers = Self::batched(ctx, FINGERPRINT_DOCUMENT, numbers, class, cancel)?;
                Payload::Fingerprints(
                    numbers
                        .iter()
                        .filter_map(|number| {
                            answers
                                .get(number)
                                .filter(|node| node.is_object())
                                .map(|node| (*number, fingerprint(node)))
                        })
                        .collect(),
                )
            }
            Read::ArtifactFiles(_) | Read::ArtifactFile { .. } => {
                let response = ctx.ok(&requests[0], cancel)?;
                super::artifact_payload(read, &response.body)?
            }
            Read::Timeline(_) => Payload::Timeline(
                Self::rest_pages(ctx, requests.remove(0), cancel)?
                    .iter()
                    .map(timeline_event)
                    .collect(),
            ),
            Read::Threads(number) => Payload::Threads(
                Self::connection(ctx, THREADS_DOCUMENT, *number, "reviewThreads", cancel)?
                    .iter()
                    .map(thread)
                    .collect(),
            ),
            Read::Viewed(number) => Payload::Viewed(
                Self::connection(ctx, FILES_DOCUMENT, *number, "files", cancel)?
                    .iter()
                    .map(|file| (text(file, "path"), file["viewerViewedState"] == "VIEWED"))
                    .collect(),
            ),
            Read::Diff(_) | Read::CommitDiff(_) => {
                Payload::Text(Self::text_read(ctx, requests.remove(0), cancel)?)
            }
            Read::PullRequestForBranch(_) => Payload::MaybePullRequest(
                ctx.json(&requests[0], cancel)?
                    .as_array()
                    .and_then(|pulls| pulls.first())
                    .and_then(|pull| pull["number"].as_u64()),
            ),
            Read::Workflows => {
                let value = ctx.json(&requests[0], cancel)?;
                Payload::Workflows(
                    value["workflows"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(workflow)
                        .collect(),
                )
            }
            Read::WorkflowSource { reference, .. } => {
                let definition = ctx.json(&requests[0], cancel)?;
                let path = text(&definition, "path");
                if path.is_empty() {
                    return Err(ForgeError::BadResponse("workflow has no path".into()));
                }
                let mut request = repos_path(ctx, &format!("contents/{path}"))
                    .map(ApiRequest::get)?
                    .accept(Accept::Raw);
                if let Some(reference) =
                    reference.as_ref().filter(|reference| !reference.is_empty())
                {
                    request = request.query("ref", reference);
                }
                Payload::Text(Self::text_read(ctx, request, cancel)?)
            }
            Read::Runs { page, .. } => {
                let value = ctx.json(&requests[0], cancel)?;
                let items = value["workflow_runs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(run)
                    .collect::<Vec<_>>();
                let total = value["total_count"].as_u64();
                Payload::Runs(PageOf {
                    next: total
                        .is_some_and(|total| total > (page.number * page.per_page) as u64)
                        .then_some(Page {
                            number: page.number + 1,
                            per_page: page.per_page,
                        }),
                    items,
                    total,
                    other_total: None,
                })
            }
            Read::Run { attempt, .. } => {
                let run_value = ctx.json(&requests[0], cancel)?;
                let jobs = Self::rest_pages(ctx, requests.remove(1), cancel)?
                    .into_iter()
                    .flat_map(|page| page["jobs"].as_array().cloned().unwrap_or_default())
                    .map(|value| job(&value))
                    .collect::<Vec<_>>();
                let mut run = run(&run_value);
                if attempt.is_none() {
                    run.attempts = run.attempt;
                }
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
            Read::Job(_) => Payload::Job(Box::new(job(&ctx.json(&requests[0], cancel)?))),
            Read::JobLog(id) => {
                let request = requests.remove(0);
                let cap = request.body_cap;
                let response = ctx.send(&request, cancel);
                let (text, truncated) = match response {
                    Ok(response) if response.success() => (response.text(), false),
                    Ok(response) => {
                        return Err(crate::http::classify(&ctx.endpoints, &response));
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
                Payload::Log(Log::new(id.clone(), text, LogFormat::Plain, truncated))
            }
            Read::Artifacts(_) => {
                let value = ctx.json(&requests[0], cancel)?;
                Payload::Artifacts(
                    value["artifacts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(artifact)
                        .collect(),
                )
            }
            Read::Notifications { page, .. } => {
                let response = ctx.ok(&requests[0], cancel)?;
                let next = response.next_link().map(|_| Page {
                    number: page.number + 1,
                    per_page: page.per_page,
                });
                Payload::Notifications(PageOf {
                    items: response
                        .json()?
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(notification)
                        .collect(),
                    next,
                    total: None,
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

    fn milestone_number(
        ctx: &ForgeContext,
        title: &str,
        cancel: &AtomicBool,
    ) -> Result<u64, ForgeError> {
        if let Ok(number) = title.trim().parse::<u64>() {
            return Ok(number);
        }
        let request = ApiRequest::get(repos_path(ctx, "milestones")?)
            .query("state", "all")
            .query("per_page", PAGE);
        Self::rest_pages(ctx, request, cancel)?
            .iter()
            .find(|milestone| text(milestone, "title").eq_ignore_ascii_case(title.trim()))
            .and_then(|milestone| milestone["number"].as_u64())
            .ok_or_else(|| ForgeError::NotFound(format!("milestone {title}")))
    }

    fn send_write(
        ctx: &ForgeContext,
        request: ApiRequest,
        cancel: &AtomicBool,
    ) -> Result<Value, ForgeError> {
        ctx.json(&request, cancel)
    }

    /// A suggested change committed the way GitHub's "Commit suggestion" does, through the contents API: the head branch's file is read, the suggested lines
    /// replace the ones the suggestion was made on (checked first), and the file is written back
    /// on that branch, pinned to the blob that was read.
    fn commit_suggestion(
        ctx: &ForgeContext,
        number: u64,
        suggestion: &Suggestion<'_>,
        head: &str,
        message: &str,
        cancel: &AtomicBool,
    ) -> Result<(), ForgeError> {
        let pull = ctx.json(
            &ApiRequest::get(repos_path(ctx, &format!("pulls/{number}"))?),
            cancel,
        )?;
        let sha = text(&pull["head"], "sha");
        if !head.is_empty() && sha != head {
            return Err(ForgeError::Conflict(
                "The pull request has new commits · refresh before committing this suggestion"
                    .into(),
            ));
        }
        let branch = text(&pull["head"], "ref");
        let repo = match pull["head"]["repo"]["full_name"].as_str() {
            Some(name) => name.to_owned(),
            None => {
                let repo = ctx.repo()?;
                format!("{}/{}", repo.owner, repo.name)
            }
        };
        let contents = format!(
            "repos/{repo}/contents/{}",
            suggestion
                .path
                .split('/')
                .map(path_segment)
                .collect::<Vec<_>>()
                .join("/")
        );
        let file = ctx.json(
            &ApiRequest::get(contents.clone()).query("ref", &branch),
            cancel,
        )?;
        let decoded = base64_decode(&text(&file, "content"))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| ForgeError::Failed(format!("{} is not a text file", suggestion.path)))?;
        let changed = suggestion.apply(&decoded).ok_or_else(|| {
            ForgeError::Conflict(format!(
                "Lines {}–{} of {} changed since the suggestion · refresh and look again",
                suggestion.lines.0, suggestion.lines.1, suggestion.path
            ))
        })?;
        let request = ApiRequest::new(Method::Put, contents).json(json!({
            "message": message,
            "content": base64_encode(changed.as_bytes()),
            "sha": text(&file, "sha"),
            "branch": branch,
        }));
        Self::send_write(ctx, request, cancel)?;
        Ok(())
    }

    fn mutation(
        ctx: &ForgeContext,
        document: &str,
        variables: Value,
        cancel: &AtomicBool,
    ) -> Result<Value, ForgeError> {
        let response = ctx.send(&ApiRequest::graphql(document, variables), cancel)?;
        if !response.success() {
            return Err(crate::http::classify(&ctx.endpoints, &response));
        }
        let body = response.json()?;
        if let Some(error) = body["errors"].as_array().and_then(|errors| errors.first()) {
            let message = crate::cli::redact(error["message"].as_str().unwrap_or("GraphQL error"));
            return Err(match error["type"].as_str() {
                Some("NOT_FOUND") => ForgeError::NotFound(message),
                Some("FORBIDDEN") => ForgeError::Forbidden(message),
                Some("RATE_LIMITED") => ForgeError::RateLimited {
                    until: std::time::Instant::now() + std::time::Duration::from_secs(60),
                },
                _ => ForgeError::Failed(message),
            });
        }
        Ok(body["data"].clone())
    }

    /// The `ApiRequest`s a write sends, in order (GraphQL writes use `mutation`).
    pub fn write_requests(
        ctx: &ForgeContext,
        write: &Write,
    ) -> Result<Vec<ApiRequest>, ForgeError> {
        let path = |tail: &str| repos_path(ctx, tail);
        let post = |tail: &str, body: Value| -> Result<ApiRequest, ForgeError> {
            Ok(ApiRequest::new(Method::Post, path(tail)?).json(body))
        };
        let issue_or_pull = |kind: ItemKind, number: u64| match kind {
            ItemKind::Issue => format!("issues/{number}"),
            ItemKind::PullRequest => format!("pulls/{number}"),
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
                    payload["labels"] = json!(labels);
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
                let mut payload = json!({
                    "title": request.title,
                    "body": request.body,
                    "base": request.base_branch,
                    "head": format!("{owner}:{}", request.head_branch),
                    "draft": request.draft,
                });
                if request.head_repository != request.base_repository
                    && let Some((_, name)) = request.head_repository.split_once('/')
                {
                    // Unlike `gh pr create --head`, REST disambiguates organization-owned forks.
                    payload["head_repo"] = name.into();
                }
                vec![
                    ApiRequest::new(
                        Method::Post,
                        format!("repos/{}/pulls", request.base_repository),
                    )
                    .json(payload)
                    .body_file(),
                ]
            }
            Write::Edit {
                number,
                title,
                body,
                ..
            } => vec![
                ApiRequest::new(Method::Patch, path(&format!("issues/{number}"))?)
                    .json(json!({ "title": title, "body": body })),
            ],
            Write::Comment { number, body, .. } => {
                vec![post(
                    &format!("issues/{number}/comments"),
                    json!({ "body": body }),
                )?]
            }
            Write::SetState {
                kind,
                number,
                open,
                reason,
            } => {
                let mut payload = json!({ "state": if *open { "open" } else { "closed" } });
                if *kind == ItemKind::Issue && !*open {
                    payload["state_reason"] = match reason {
                        Some(ClosedReason::NotPlanned) => "not_planned",
                        Some(ClosedReason::Duplicate) => "duplicate",
                        _ => "completed",
                    }
                    .into();
                }
                vec![
                    ApiRequest::new(Method::Patch, path(&issue_or_pull(*kind, *number))?)
                        .json(payload),
                ]
            }
            Write::SetLabels {
                number,
                add,
                remove,
                ..
            } => {
                let mut requests = Vec::new();
                for label in remove {
                    requests.push(ApiRequest::new(
                        Method::Delete,
                        path(&format!("issues/{number}/labels/{}", path_segment(label)))?,
                    ));
                }
                if !add.is_empty() {
                    requests.push(post(
                        &format!("issues/{number}/labels"),
                        json!({ "labels": add }),
                    )?);
                }
                requests
            }
            Write::SetAssignees {
                number,
                add,
                remove,
                ..
            } => {
                let mut requests = Vec::new();
                if !remove.is_empty() {
                    requests.push(
                        ApiRequest::new(
                            Method::Delete,
                            path(&format!("issues/{number}/assignees"))?,
                        )
                        .json(json!({ "assignees": remove })),
                    );
                }
                if !add.is_empty() {
                    requests.push(post(
                        &format!("issues/{number}/assignees"),
                        json!({ "assignees": add }),
                    )?);
                }
                requests
            }
            Write::SetMilestone { number, .. } => vec![
                ApiRequest::new(Method::Patch, path(&format!("issues/{number}"))?)
                    .json(json!({ "milestone": Value::Null })),
            ],
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
            Write::InlineComment {
                number,
                path: file,
                line,
                side,
                head,
                body,
            } => vec![post(
                &format!("pulls/{number}/comments"),
                json!({ "body": body, "commit_id": head, "path": file, "line": line, "side": side.as_github() }),
            )?],
            Write::SubmitReview {
                number,
                verdict,
                body,
                drafts,
                head,
            } => {
                let mut payload = json!({
                    "event": match verdict {
                        Verdict::Approve => "APPROVE",
                        Verdict::RequestChanges => "REQUEST_CHANGES",
                        Verdict::Comment => "COMMENT",
                    },
                });
                if !body.is_empty() {
                    payload["body"] = body.clone().into();
                }
                if let Some(head) = head {
                    payload["commit_id"] = head.clone().into();
                }
                if !drafts.is_empty() {
                    payload["comments"] = drafts
                        .iter()
                        .map(|draft| {
                            let mut comment = json!({ "path": draft.path, "line": draft.line, "side": draft.side.as_github(), "body": draft.body });
                            // A multi-line comment: GitHub anchors the whole range.
                            if let Some((start, _)) = draft.range() {
                                comment["start_line"] = start.into();
                                comment["start_side"] = draft.side.as_github().into();
                            }
                            comment
                        })
                        .collect();
                }
                // The review's text can be long: over CLI passthrough it travels in a file.
                vec![post(&format!("pulls/{number}/reviews"), payload)?.body_file()]
            }
            Write::Merge {
                number,
                method,
                head,
                title,
                message,
                ..
            } => {
                let mut payload = json!({
                    "sha": head,
                    "merge_method": match method {
                        MergeMethod::Squash => "squash",
                        MergeMethod::Rebase | MergeMethod::RebaseMerge | MergeMethod::FastForwardOnly => "rebase",
                        MergeMethod::Merge => "merge",
                    },
                });
                if let Some(title) = title {
                    payload["commit_title"] = title.clone().into();
                }
                if let Some(message) = message {
                    payload["commit_message"] = message.clone().into();
                }
                vec![
                    ApiRequest::new(Method::Put, path(&format!("pulls/{number}/merge"))?)
                        .json(payload),
                ]
            }
            Write::Dispatch {
                workflow,
                reference,
                inputs,
            } => {
                let inputs = inputs
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect::<serde_json::Map<_, _>>();
                vec![post(
                    &format!("actions/workflows/{workflow}/dispatches"),
                    json!({ "ref": reference, "inputs": inputs }),
                )?]
            }
            Write::Rerun { run, scope, debug } => vec![post(
                &match scope {
                    RerunScope::All => format!("actions/runs/{run}/rerun"),
                    RerunScope::Failed => format!("actions/runs/{run}/rerun-failed-jobs"),
                },
                json!({ "enable_debug_logging": debug }),
            )?],
            Write::RerunJob(job) => vec![post(&format!("actions/jobs/{job}/rerun"), json!({}))?],
            Write::CancelRun(run) => {
                vec![ApiRequest::new(
                    Method::Post,
                    path(&format!("actions/runs/{run}/cancel"))?,
                )]
            }
            Write::ApproveRun { run, .. } => {
                vec![ApiRequest::new(
                    Method::Post,
                    path(&format!("actions/runs/{run}/approve"))?,
                )]
            }
            Write::MarkRead(thread) => vec![ApiRequest::new(
                Method::Patch,
                format!("notifications/threads/{thread}"),
            )],
            Write::MarkAllRead => {
                vec![ApiRequest::new(Method::Put, "notifications").json(json!({}))]
            }
            Write::Unsubscribe(thread) => vec![ApiRequest::new(
                Method::Delete,
                format!("notifications/threads/{thread}/subscription"),
            )],
            Write::Reply { .. }
            | Write::Resolve { .. }
            | Write::CommitSuggestion { .. }
            | Write::SetViewed { .. }
            | Write::Subscribe { .. } => Vec::new(),
            Write::Checkout { .. }
            | Write::Worktree { .. }
            | Write::Develop { .. }
            | Write::SetProjects { .. }
            | Write::OpenInBrowser { .. } => {
                return Err(ForgeError::Unsupported {
                    capability: "porcelain",
                    reason: "runs through the forge CLI, not the API".into(),
                });
            }
            Write::DownloadArtifact { artifact, .. } => vec![super::download(ApiRequest::get(
                path(&format!("actions/artifacts/{artifact}/zip"))?,
            ))],
        })
    }
}

impl Provider for GitHub {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitHub
    }

    fn terms(&self) -> &'static Terms {
        &GITHUB_TERMS
    }

    fn capabilities(&self, ctx: &ForgeContext) -> Capabilities {
        let capabilities = Capabilities::github(ctx.transport().graphql_batching());
        if ctx.anonymous() {
            capabilities.read_only(ProviderKind::GitHub)
        } else {
            capabilities
        }
    }

    fn incremental(&self, _ctx: &ForgeContext, read: &Read) -> bool {
        matches!(read, Read::Notifications { filter, page } if filter.since.is_none() && page.number <= 1)
    }

    fn plan(&self, ctx: &ForgeContext, read: &Read, _cond: &Conditional) -> Plan {
        match Self::requests(ctx, read) {
            Ok(requests) if requests.is_empty() => Plan {
                requests,
                note: Some("read locally from the base branch"),
            },
            Ok(requests) => Plan {
                requests,
                note: None,
            },
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
        ctx.require_credential(write, false)?;
        let invalidates = write.invalidates();
        let saved = |summary: &str| Receipt {
            summary: summary.into(),
            web_url: None,
            created_number: None,
            invalidates: invalidates.clone(),
        };
        match write {
            Write::Reply { thread, body, .. } => {
                Self::mutation(
                    ctx,
                    REPLY_DOCUMENT,
                    json!({ "thread": thread.0, "body": body }),
                    cancel,
                )?;
                return Ok(saved("Reply saved"));
            }
            Write::Resolve { thread, resolved } => {
                Self::mutation(
                    ctx,
                    if *resolved {
                        RESOLVE_DOCUMENT
                    } else {
                        UNRESOLVE_DOCUMENT
                    },
                    json!({ "thread": thread.0 }),
                    cancel,
                )?;
                return Ok(saved(if *resolved { "Resolved" } else { "Unresolved" }));
            }
            Write::CommitSuggestion {
                number,
                path,
                start,
                end,
                original,
                replacement,
                head,
                message,
            } => {
                let suggestion = Suggestion {
                    path,
                    lines: (*start, *end),
                    original,
                    replacement,
                };
                Self::commit_suggestion(ctx, *number, &suggestion, head, message, cancel)?;
                return Ok(saved("Suggestion committed"));
            }
            Write::SetViewed {
                node, path, viewed, ..
            } => {
                Self::mutation(
                    ctx,
                    if *viewed {
                        VIEWED_DOCUMENT
                    } else {
                        UNVIEWED_DOCUMENT
                    },
                    json!({ "pullRequest": node.0, "path": path }),
                    cancel,
                )?;
                return Ok(saved("Saved"));
            }
            Write::Subscribe { node, state, .. } => {
                Self::mutation(
                    ctx,
                    SUBSCRIPTION_DOCUMENT,
                    json!({ "subject": node.0, "state": state.as_github() }),
                    cancel,
                )?;
                return Ok(saved("Saved"));
            }
            Write::ApproveRun { run, expected } => {
                Self::approve(ctx, run, expected, cancel)?;
                return Ok(saved("Approved"));
            }
            Write::DownloadArtifact {
                artifact,
                name,
                directory,
            } => {
                let request = super::download(ApiRequest::get(repos_path(
                    ctx,
                    &format!("actions/artifacts/{artifact}/zip"),
                )?));
                let response = ctx.ok(&request, cancel)?;
                return super::save_artifact(&response.body, name, directory);
            }
            Write::SetMilestone {
                number, milestone, ..
            } => {
                let value = match milestone
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                {
                    Some(title) => Value::from(Self::milestone_number(ctx, title, cancel)?),
                    None => Value::Null,
                };
                let request =
                    ApiRequest::new(Method::Patch, repos_path(ctx, &format!("issues/{number}"))?)
                        .json(json!({ "milestone": value }));
                Self::send_write(ctx, request, cancel)?;
                return Ok(saved("Saved"));
            }
            _ => {}
        }
        let mut last = Value::Null;
        for request in Self::write_requests(ctx, write)? {
            if cancel.load(Ordering::Acquire) {
                return Err(ForgeError::Cancelled);
            }
            last = Self::send_write(ctx, request, cancel)?;
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

/// A suggested change: lines `lines.0..=lines.1` of `path`, which read `original` (when known),
/// become `replacement`.
struct Suggestion<'a> {
    path: &'a str,
    lines: (u64, u64),
    original: &'a [String],
    replacement: &'a [String],
}

impl Suggestion<'_> {
    /// `file` with the lines swapped, each new line ending as the last replaced one did;
    /// `None` when the lines are out of range or no longer read as the suggestion saw them.
    fn apply(&self, file: &str) -> Option<String> {
        let (start, end) = self.lines;
        let lines = file.split_inclusive('\n').collect::<Vec<_>>();
        if start == 0 || start > end || end as usize > lines.len() {
            return None;
        }
        let (start, end) = (start as usize - 1, end as usize);
        let bare = |line: &str| {
            line.trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_owned()
        };
        if !self.original.is_empty()
            && lines[start..end]
                .iter()
                .map(|line| bare(line))
                .ne(self.original.iter().cloned())
        {
            return None;
        }
        let ending = |line: &str| line[bare(line).len()..].to_owned();
        // The last line of the file keeps having (or not having) its newline; the others end
        // as the file's lines there do.
        let last = ending(lines[end - 1]);
        let between = Some(ending(lines[start]))
            .filter(|ending| !ending.is_empty())
            .unwrap_or_else(|| "\n".into());
        let mut changed = lines[..start].concat();
        for (index, line) in self.replacement.iter().enumerate() {
            changed.push_str(line);
            changed.push_str(if index + 1 == self.replacement.len() {
                &last
            } else {
                &between
            });
        }
        changed.push_str(&lines[end..].concat());
        Some(changed)
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if index <= chunk.len() {
                BASE64[(n >> shift) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Standard base64, line breaks and padding allowed (the contents API wraps at 60 columns).
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
    {
        let value = BASE64.iter().position(|known| *known == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Endpoints;
    use crate::transport::HostKey;

    fn ctx() -> ForgeContext {
        ForgeContext::offline(
            Endpoints::github(&HostKey::new("github.com")),
            RepoRef::parse("orbit/orbit"),
        )
    }

    #[test]
    fn a_suggestion_replaces_its_lines_and_keeps_line_endings() {
        let original = vec!["b".to_owned(), "c".to_owned()];
        let replacement = vec!["B".to_owned(), "C".to_owned(), "D".to_owned()];
        let suggestion = Suggestion {
            path: "x.py",
            lines: (2, 3),
            original: &original,
            replacement: &replacement,
        };
        assert_eq!(
            suggestion.apply("a\nb\nc\nd\n").as_deref(),
            Some("a\nB\nC\nD\nd\n")
        );
        assert_eq!(
            suggestion.apply("a\r\nb\r\nc").as_deref(),
            Some("a\r\nB\r\nC\r\nD")
        );
        // The lines moved on since the suggestion: nothing is committed.
        assert_eq!(suggestion.apply("a\nb\nX\nd\n"), None);
        assert_eq!(suggestion.apply("a\nb\n"), None);
        // A suggestion with no lines deletes them.
        let delete = Suggestion {
            replacement: &[],
            ..suggestion
        };
        assert_eq!(delete.apply("a\nb\nc\nd\n").as_deref(), Some("a\nd\n"));
    }

    #[test]
    fn base64_round_trips_with_the_contents_apis_line_breaks() {
        for text in ["", "a", "ab", "abc", "retry_delay(attempt) → 60\n"] {
            let encoded = base64_encode(text.as_bytes());
            assert_eq!(
                base64_decode(&encoded).as_deref(),
                Some(text.as_bytes()),
                "{encoded}"
            );
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TW\nFu\n").as_deref(), Some(&b"Man"[..]));
        assert_eq!(base64_decode("T!=="), None);
    }

    #[test]
    fn list_queries_keep_user_terms_and_translate_state() {
        let repo = RepoRef::parse("orbit/orbit").unwrap();
        let filter = Filter {
            state: StateFilter::Merged,
            query: "label:bug".into(),
        };
        assert_eq!(
            GitHub::search_query(Some(&repo), ItemKind::PullRequest, &filter, "created-desc"),
            "repo:orbit/orbit is:pr is:merged label:bug sort:created-desc"
        );
        let filter = Filter {
            state: StateFilter::All,
            query: "sort:updated-asc".into(),
        };
        assert_eq!(
            GitHub::search_query(None, ItemKind::Issue, &filter, "updated-desc"),
            "is:issue sort:updated-asc"
        );
    }

    #[test]
    fn every_read_has_a_plan_with_named_graphql_operations() {
        let ctx = ctx();
        for read in super::super::sample_reads() {
            let plan = GitHub.plan(&ctx, &read, &Conditional::default());
            for request in &plan.requests {
                if request.target == Target::GraphQl {
                    assert!(request.graphql_operation().is_some(), "{read:?}");
                }
            }
        }
        let plan = GitHub.plan(&ctx, &Read::Diff(9), &Conditional::default());
        assert_eq!(plan.requests[0].describe(), "GET repos/orbit/orbit/pulls/9");
        assert_eq!(plan.requests[0].accept, Accept::Diff);
    }

    #[test]
    fn writes_send_json_bodies_never_argv() {
        let ctx = ctx();
        let requests = GitHub::write_requests(
            &ctx,
            &Write::SetLabels {
                kind: ItemKind::Issue,
                number: 7,
                add: vec!["native".into()],
                remove: vec!["needs triage".into()],
            },
        )
        .unwrap();
        let described = requests
            .iter()
            .map(ApiRequest::describe)
            .collect::<Vec<_>>();
        assert_eq!(
            described,
            [
                "DELETE repos/orbit/orbit/issues/7/labels/needs%20triage",
                "POST repos/orbit/orbit/issues/7/labels"
            ]
        );
        let merge = GitHub::write_requests(
            &ctx,
            &Write::Merge {
                number: 9,
                method: MergeMethod::Squash,
                head: "a".repeat(40),
                delete_branch: false,
                title: None,
                message: None,
            },
        )
        .unwrap();
        assert_eq!(merge[0].method, Method::Put);
        let Some(crate::transport::Body::Json(body)) = &merge[0].body else {
            panic!("merge has a JSON body");
        };
        assert_eq!(body["sha"], "a".repeat(40));
        assert_eq!(body["merge_method"], "squash");
    }

    #[test]
    fn details_are_read_again_without_projects_when_the_scope_is_missing() {
        for document in [ISSUE_DOCUMENT, PULL_REQUEST_DOCUMENT] {
            assert!(document.contains(PROJECT_ITEMS));
            let stripped = without_projects(document);
            assert!(!stripped.contains("projectItems"));
            assert!(stripped.contains("milestone { number title dueOn state } viewerSubscription"));
        }
        // What github.com answers a gh token (no `read:project`) for the detail queries.
        let refused = json!({
            "errors": [{ "type": "INSUFFICIENT_SCOPES", "message": "Your token has not been \
                granted the required scopes to execute this query. The 'title' field requires \
                one of the following scopes: ['read:project'], but your token has only been \
                granted the: ['gist', 'read:org', 'repo'] scopes." }]
        });
        let error = graphql_data(refused).unwrap_err();
        assert!(needs_project_scope(&error), "{error:?}");
        assert!(!needs_project_scope(&ForgeError::Forbidden(
            "Resource not accessible".into()
        )));
    }

    #[test]
    fn graphql_partial_errors_keep_data_and_missing_data_classifies() {
        let partial = json!({
            "data": { "repository": { "issue": { "number": 7, "projectItems": null } } },
            "errors": [{ "type": "INSUFFICIENT_SCOPES", "message": "read:project" }]
        });
        assert!(graphql_data(partial).is_ok());
        let missing = json!({
            "data": { "repository": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve" }]
        });
        assert!(matches!(
            graphql_data(missing),
            Err(ForgeError::NotFound(_))
        ));
        let limited = json!({ "errors": [{ "type": "RATE_LIMITED", "message": "slow down" }] });
        assert!(matches!(
            graphql_data(limited),
            Err(ForgeError::RateLimited { .. })
        ));
    }

    #[test]
    fn rows_expand_check_counts_and_lifecycle() {
        let node = json!({
            "number": 9, "title": "t", "state": "OPEN", "isDraft": true, "reviewDecision": "APPROVED",
            "comments": {"totalCount": 3}, "labels": {"nodes": [{"name": "bug"}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE", "contexts": {
                "checkRunCountsByState": [{"state": "SUCCESS", "count": 4}, {"state": "FAILURE", "count": 1}],
                "statusContextCountsByState": [{"state": "SUCCESS", "count": 1}]
            }}}}]}
        });
        let row = row(&node, ItemKind::PullRequest);
        assert_eq!(row.lifecycle, Lifecycle::Draft);
        assert_eq!(row.review, Some(ReviewState::Approved));
        assert_eq!(row.ci, Some(CiState::Failed));
        assert_eq!(row.ci_counts, [(CiState::Passed, 5), (CiState::Failed, 1)]);
        assert_eq!(row.comments, 3);
    }
}
