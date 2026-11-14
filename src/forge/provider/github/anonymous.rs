//! GitHub without a credential (FORGE.md §2.5): REST only, since GitHub's GraphQL API refuses
//! anonymous callers. Each REST answer is reshaped into the node the GraphQL documents return,
//! so the mappers in `github.rs` (`row`, `pull_request`, `issue`, `thread`) stay the only
//! place that knows the model.
//!
//! The quota is 60 requests an hour per address, so detail reads fetch at most a few pages,
//! every GET revalidates with its ETag (an anonymous 304 is free), background batches send
//! nothing, and reads that need a sign-in (job logs, notifications, artifact archives, the
//! viewer, `@me` queries, viewed files) answer without a request.
//!
//! Gaps against the GraphQL path: no review decision, no closing/linked issues, no projects or
//! subscription state, no viewed-file state, check runs without their workflow name, thread
//! resolution unknown, and comment counts missing from pull request rows.

use super::*;

/// Detail reads follow at most this many REST pages (100 items each).
const DETAIL_PAGES: usize = 3;

/// The anonymous answer to `read`, or `None` when the authenticated path is already REST and
/// works without a credential (repository, labels, timeline, diffs, Actions).
pub(super) fn fetch(
    ctx: &ForgeContext,
    read: &Read,
    cond: &Conditional,
    cancel: &AtomicBool,
) -> Option<Result<Fetched, ForgeError>> {
    Some(match read {
        Read::Viewer => Err(ctx.sign_in_to("see your account")),
        Read::Notifications { .. } => Err(ctx.sign_in_to("see notifications")),
        Read::JobLog(_) => Err(ctx.sign_in_to("read logs")),
        Read::ArtifactFiles(_) | Read::ArtifactFile { .. } => {
            Err(ctx.sign_in_to("download artifacts"))
        }
        // Viewed state lives in GraphQL only; anonymously nothing is marked.
        Read::Viewed(_) => Ok(Fetched::payload(Payload::Viewed(Vec::new()))),
        Read::List { kind, filter, page } => {
            list(ctx, *kind, filter, *page, cancel).map(|rows| Fetched::payload(Payload::Rows(rows)))
        }
        Read::Search { kind, filter, page } => search(ctx, None, *kind, filter, *page, cancel)
            .map(|rows| Fetched::payload(Payload::Rows(rows))),
        Read::Issue(number) => issue_detail(ctx, *number, cond, cancel),
        Read::PullRequest(number) => pull_detail(ctx, *number, cancel).map(|pull| {
            let fingerprint = Some(pull.fingerprint.clone());
            Fetched::Payload {
                value: Payload::PullRequest(Box::new(pull)),
                etag: None,
                fingerprint,
            }
        }),
        Read::Fingerprint(number) => fingerprint_of(ctx, *number, cancel).map(|fingerprint| {
            if cond.fingerprint.as_ref() == Some(&fingerprint) {
                Fetched::NotModified
            } else {
                Fetched::payload(Payload::Fingerprint(fingerprint))
            }
        }),
        // A prefetch nobody waits for would spend several requests per pull request.
        Read::PullRequests(_) if ctx.background() => {
            Ok(Fetched::payload(Payload::PullRequests(Vec::new())))
        }
        Read::PullRequests(numbers) => numbers
            .iter()
            .filter_map(|number| match pull_detail(ctx, *number, cancel) {
                Err(ForgeError::NotFound(_)) => None,
                result => Some(result),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|pulls| Fetched::payload(Payload::PullRequests(pulls))),
        Read::Fingerprints(numbers) => numbers
            .iter()
            .filter_map(|number| match fingerprint_of(ctx, *number, cancel) {
                Err(ForgeError::NotFound(_)) => None,
                result => Some(result.map(|fingerprint| (*number, fingerprint))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|prints| Fetched::payload(Payload::Fingerprints(prints))),
        Read::Threads(number) => {
            threads(ctx, *number, cancel).map(|threads| Fetched::payload(Payload::Threads(threads)))
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------------------ reshaping

fn upper(value: &Value) -> Value {
    value
        .as_str()
        .map(|text| Value::from(text.to_ascii_uppercase()))
        .unwrap_or(Value::Null)
}

fn login(user: &Value) -> Value {
    if user.is_object() {
        json!({ "login": user["login"] })
    } else {
        Value::Null
    }
}

fn logins(list: &Value) -> Value {
    json!({ "nodes": list.as_array().into_iter().flatten().map(login).collect::<Vec<_>>() })
}

fn label_nodes(list: &Value) -> Value {
    json!({
        "nodes": list
            .as_array()
            .into_iter()
            .flatten()
            .map(|label| json!({ "name": label["name"], "color": label["color"] }))
            .collect::<Vec<_>>()
    })
}

fn milestone_node(value: &Value) -> Value {
    if !value.is_object() {
        return Value::Null;
    }
    json!({
        "number": value["number"],
        "title": value["title"],
        "dueOn": value["due_on"],
        "state": upper(&value["state"]),
    })
}

/// `o/r` from a pull's base, or from an issue's `repository_url`.
fn repository(item: &Value) -> Value {
    item["base"]["repo"]["full_name"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            item["repository_url"]
                .as_str()
                .and_then(|url| url.split("/repos/").nth(1))
                .map(str::to_owned)
        })
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn lifecycle_word(item: &Value) -> &'static str {
    if item["merged_at"].is_string() || item["pull_request"]["merged_at"].is_string() {
        "MERGED"
    } else if item["state"] == "closed" {
        "CLOSED"
    } else {
        "OPEN"
    }
}

/// A REST issue, pull, or search item as the GraphQL list node `row()` reads.
fn item_node(item: &Value) -> Value {
    let pull = item.get("pull_request").is_some() || item.get("head").is_some();
    json!({
        "__typename": if pull { "PullRequest" } else { "Issue" },
        "id": item["node_id"],
        "number": item["number"],
        "title": item["title"],
        "body": item["body"],
        "state": lifecycle_word(item),
        "stateReason": upper(&item["state_reason"]),
        "url": item["html_url"],
        "createdAt": item["created_at"],
        "updatedAt": item["updated_at"],
        "author": login(&item["user"]),
        "labels": label_nodes(&item["labels"]),
        "comments": { "totalCount": item["comments"] },
        "repository": { "nameWithOwner": repository(item) },
        "isDraft": item["draft"],
        "headRefName": item["head"]["ref"],
        "assignees": logins(&item["assignees"]),
        "milestone": milestone_node(&item["milestone"]),
    })
}

fn check_run_node(run: &Value) -> Value {
    let details = run["details_url"]
        .as_str()
        .or_else(|| run["html_url"].as_str())
        .unwrap_or_default();
    // `…/actions/runs/<run>/job/<job>`; the job id is read from the URL by `check()`.
    let run_id = details
        .split("/actions/runs/")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?']).next())
        .and_then(|id| id.parse::<u64>().ok());
    json!({
        "__typename": "CheckRun",
        "name": run["name"],
        "status": upper(&run["status"]),
        "conclusion": upper(&run["conclusion"]),
        "startedAt": run["started_at"],
        "completedAt": run["completed_at"],
        "detailsUrl": details,
        "checkSuite": { "workflowRun": run_id.map(|id| json!({ "databaseId": id })) },
    })
}

fn status_node(status: &Value) -> Value {
    json!({
        "__typename": "StatusContext",
        "context": status["context"],
        "state": upper(&status["state"]),
        "targetUrl": status["target_url"],
        "createdAt": status["created_at"],
    })
}

fn commit_node(commit: &Value) -> Value {
    let message = commit["commit"]["message"].as_str().unwrap_or_default();
    let (headline, body) = message.split_once('\n').unwrap_or((message, ""));
    json!({
        "commit": {
            "oid": commit["sha"],
            "messageHeadline": headline.trim(),
            "messageBody": body.trim(),
            "committedDate": commit["commit"]["committer"]["date"],
            "author": { "user": login(&commit["author"]) },
        }
    })
}

fn review_node(review: &Value) -> Value {
    json!({
        "id": review["node_id"],
        "author": login(&review["user"]),
        "state": review["state"],
        "body": review["body"],
        "submittedAt": review["submitted_at"],
    })
}

/// A REST pull with its reviews, commits and the head's checks as `PullRequestDetail`'s node.
fn pull_node(pull: &Value, reviews: &[Value], commits: &[Value], contexts: Vec<Value>) -> Value {
    let mut node = item_node(pull);
    let requested = pull["requested_reviewers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|user| json!({ "requestedReviewer": { "login": user["login"] } }))
        .chain(
            pull["requested_teams"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|team| json!({ "requestedReviewer": { "slug": team["slug"] } })),
        )
        .collect::<Vec<_>>();
    let fields = json!({
        "baseRefName": pull["base"]["ref"],
        "baseRefOid": pull["base"]["sha"],
        "headRefOid": pull["head"]["sha"],
        "headRepository": { "nameWithOwner": pull["head"]["repo"]["full_name"] },
        "mergeable": match pull["mergeable"].as_bool() {
            Some(true) => "MERGEABLE",
            Some(false) => "CONFLICTING",
            None => "UNKNOWN",
        },
        "mergeStateStatus": upper(&pull["mergeable_state"]),
        "additions": pull["additions"],
        "deletions": pull["deletions"],
        "changedFiles": pull["changed_files"],
        "reviewRequests": { "nodes": requested },
        // GraphQL asks for the last 50.
        "reviews": { "nodes": reviews[reviews.len().saturating_sub(50)..].iter().map(review_node).collect::<Vec<_>>() },
        "commits": { "nodes": commits.iter().map(commit_node).collect::<Vec<_>>() },
        "latest": { "nodes": [ { "commit": { "statusCheckRollup": { "contexts": { "nodes": contexts } } } } ] },
    });
    if let (Some(node), Some(fields)) = (node.as_object_mut(), fields.as_object()) {
        node.extend(fields.clone());
    }
    node
}

// ------------------------------------------------------------------------------ requests

fn rest(ctx: &ForgeContext, tail: &str) -> Result<ApiRequest, ForgeError> {
    GitHub::repo_path(ctx, tail).map(ApiRequest::get)
}

/// Up to `limit` REST pages following `Link: rel="next"`.
fn pages(
    ctx: &ForgeContext,
    first: ApiRequest,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<Value>, ForgeError> {
    let mut request = first;
    let mut all = Vec::new();
    for _ in 0..limit {
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        let response = ctx.ok(&request, cancel)?;
        match response.json()? {
            Value::Array(items) => all.extend(items),
            other => all.push(other),
        }
        let Some(next) = response.next_link() else {
            break;
        };
        request = ApiRequest {
            target: GitHub::relative(ctx, &next),
            query: Vec::new(),
            ..request
        };
    }
    Ok(all)
}

/// The head commit's check runs and commit statuses, as rollup contexts.
fn contexts(ctx: &ForgeContext, sha: &str, cancel: &AtomicBool) -> Result<Vec<Value>, ForgeError> {
    if sha.is_empty() {
        return Ok(Vec::new());
    }
    let runs = ctx.json(
        &rest(ctx, &format!("commits/{sha}/check-runs"))?.query("per_page", PAGE),
        cancel,
    )?;
    let status = ctx.json(&rest(ctx, &format!("commits/{sha}/status"))?, cancel)?;
    Ok(runs["check_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(check_run_node)
        .chain(
            status["statuses"]
                .as_array()
                .into_iter()
                .flatten()
                .map(status_node),
        )
        .collect())
}

fn pull(ctx: &ForgeContext, number: u64, cancel: &AtomicBool) -> Result<Value, ForgeError> {
    match ctx.json(&rest(ctx, &format!("pulls/{number}"))?, cancel) {
        Err(ForgeError::NotFound(_)) => Err(ForgeError::NotFound(format!("pull request #{number}"))),
        result => result,
    }
}

fn pull_detail(
    ctx: &ForgeContext,
    number: u64,
    cancel: &AtomicBool,
) -> Result<PullRequest, ForgeError> {
    let value = pull(ctx, number, cancel)?;
    let page = |tail: &str| rest(ctx, &format!("pulls/{number}/{tail}")).map(|r| r.query("per_page", PAGE));
    let files = pages(ctx, page("files")?, DETAIL_PAGES, cancel)?;
    let reviews = pages(ctx, page("reviews")?, 1, cancel)?;
    let commits = pages(ctx, page("commits")?, 1, cancel)?;
    let contexts = contexts(ctx, value["head"]["sha"].as_str().unwrap_or_default(), cancel)?;
    let files = files
        .iter()
        .map(|file| {
            let mut changed = changed_file(&json!({
                "path": file["filename"],
                "additions": file["additions"],
                "deletions": file["deletions"],
                "changeType": upper(&file["status"]),
            }));
            changed.old_path = file["previous_filename"].as_str().map(str::to_owned);
            changed
        })
        .collect();
    Ok(pull_request(
        &pull_node(&value, &reviews, &commits, contexts),
        files,
    ))
}

/// The same fields `pull_request()` puts in its fingerprint, so a poll and the detail agree.
fn fingerprint_of(
    ctx: &ForgeContext,
    number: u64,
    cancel: &AtomicBool,
) -> Result<Fingerprint, ForgeError> {
    let value = pull(ctx, number, cancel)?;
    let contexts = contexts(ctx, value["head"]["sha"].as_str().unwrap_or_default(), cancel)?;
    Ok(pull_request(&pull_node(&value, &[], &[], contexts), Vec::new()).fingerprint)
}

/// One request, so it revalidates with the cached copy's ETag: an unchanged issue is a free 304.
fn issue_detail(
    ctx: &ForgeContext,
    number: u64,
    cond: &Conditional,
    cancel: &AtomicBool,
) -> Result<Fetched, ForgeError> {
    let mut request = rest(ctx, &format!("issues/{number}"))?;
    if let Some(etag) = &cond.etag {
        request.headers.push(("If-None-Match".into(), etag.clone()));
    }
    let response = ctx.send(&request, cancel)?;
    if response.status == 304 {
        return Ok(Fetched::NotModified);
    }
    if !response.success() {
        return Err(match crate::forge::http::classify(&ctx.endpoints, &response) {
            ForgeError::NotFound(_) => ForgeError::NotFound(format!("issue #{number}")),
            error => error,
        });
    }
    let value = response.json()?;
    // `issues/N` also answers for pull requests; GraphQL's `issue(number:)` does not.
    if value.get("pull_request").is_some() {
        return Err(ForgeError::NotFound(format!("issue #{number}")));
    }
    Ok(Fetched::Payload {
        value: Payload::Issue(Box::new(issue(&item_node(&value)))),
        etag: response.header("etag").map(str::to_owned),
        fingerprint: None,
    })
}

fn threads(
    ctx: &ForgeContext,
    number: u64,
    cancel: &AtomicBool,
) -> Result<Vec<ReviewThread>, ForgeError> {
    let mut comments = pages(
        ctx,
        rest(ctx, &format!("pulls/{number}/comments"))?.query("per_page", PAGE),
        DETAIL_PAGES,
        cancel,
    )?;
    comments.sort_by_key(|comment| comment["id"].as_u64().unwrap_or_default());
    // A reply names the comment it answers; replies always answer the thread's first comment.
    let mut threads: Vec<Value> = Vec::new();
    let mut index = std::collections::HashMap::new();
    for comment in &comments {
        let node = json!({
            "id": comment["node_id"],
            "author": login(&comment["user"]),
            "body": comment["body"],
            "path": comment["path"],
            "line": comment["line"],
            "originalLine": comment["original_line"],
            "createdAt": comment["created_at"],
            "commit": { "oid": comment["commit_id"] },
        });
        let id = comment["id"].as_u64().unwrap_or_default();
        let parent = comment["in_reply_to_id"]
            .as_u64()
            .and_then(|parent| index.get(&parent).copied());
        let at = match parent {
            Some(at) => at,
            None => {
                threads.push(json!({
                    "id": comment["node_id"],
                    "isResolved": Value::Null,
                    "path": comment["path"],
                    "line": comment["line"],
                    "originalLine": comment["original_line"],
                    "diffSide": comment["side"],
                    "comments": { "nodes": [] },
                }));
                threads.len() - 1
            }
        };
        index.insert(id, at);
        if let Some(nodes) = threads[at]["comments"]["nodes"].as_array_mut() {
            nodes.push(node);
        }
    }
    Ok(threads
        .iter()
        .map(|node| {
            let mut thread = thread(node);
            // Resolving needs GraphQL (and a sign-in).
            thread.resolvable = false;
            thread
        })
        .collect())
}

// ------------------------------------------------------------------------------ lists

/// `@me` needs to know who is asking.
fn refuse_me(ctx: &ForgeContext, query: &str) -> Result<(), ForgeError> {
    if query.contains("@me") {
        return Err(ctx.sign_in_to("filter by @me"));
    }
    Ok(())
}

/// A repository list: `pulls` / `issues` sorted by creation like the GraphQL list, or the
/// search API when the filter has terms of its own.
fn list(
    ctx: &ForgeContext,
    kind: ItemKind,
    filter: &Filter,
    page: Page,
    cancel: &AtomicBool,
) -> Result<PageOf<ItemRow>, ForgeError> {
    if !filter.query.trim().is_empty() {
        let repo = ctx.repo()?.clone();
        return search(ctx, Some(&repo), kind, filter, page, cancel);
    }
    let state = match filter.state {
        StateFilter::Open => "open",
        StateFilter::Closed | StateFilter::Merged => "closed",
        StateFilter::All => "all",
    };
    let keep = |item: &Value| match kind {
        // The issues endpoint lists pull requests too.
        ItemKind::Issue => item.get("pull_request").is_none(),
        ItemKind::PullRequest => match filter.state {
            StateFilter::Merged => item["merged_at"].is_string(),
            StateFilter::Closed => !item["merged_at"].is_string(),
            _ => true,
        },
    };
    let path = match kind {
        ItemKind::Issue => "issues",
        ItemKind::PullRequest => "pulls",
    };
    let size = page.per_page.clamp(1, PAGE) as usize;
    let want = page.per_page.max(1) as usize;
    let skip = (page.number.max(1) as usize - 1) * want;
    let mut rest_page = skip / size + 1;
    let mut offset = skip % size;
    let mut seen = 0;
    let mut items = Vec::new();
    let mut more = false;
    // Bounded by the raw rows asked for, not by the rows kept: filtering never costs pages.
    while seen < want + offset {
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        let response = ctx.ok(
            &rest(ctx, path)?
                .query("state", state)
                .query("sort", "created")
                .query("direction", "desc")
                .query("per_page", size)
                .query("page", rest_page),
            cancel,
        )?;
        let raw = response.json()?.as_array().cloned().unwrap_or_default();
        seen += raw.len();
        items.extend(
            raw.iter()
                .skip(offset)
                .filter(|item| keep(item))
                .map(|item| row(&item_node(item), kind)),
        );
        offset = 0;
        more = response.next_link().is_some();
        if !more || raw.is_empty() {
            break;
        }
        rest_page += 1;
    }
    items.truncate(want);
    Ok(PageOf {
        next: more.then_some(Page {
            number: page.number + 1,
            per_page: page.per_page,
        }),
        items,
        total: None,
    })
}

/// `GET /search/issues` (10 requests a minute without a credential, its own quota).
fn search(
    ctx: &ForgeContext,
    repo: Option<&RepoRef>,
    kind: ItemKind,
    filter: &Filter,
    page: Page,
    cancel: &AtomicBool,
) -> Result<PageOf<ItemRow>, ForgeError> {
    refuse_me(ctx, &filter.query)?;
    let sort = if repo.is_some() {
        "created-desc"
    } else {
        "updated-desc"
    };
    let query = GitHub::search_query(repo, kind, filter, sort);
    let size = page.per_page.clamp(1, PAGE);
    let value = ctx.json(
        &ApiRequest::get("search/issues")
            .query("q", &query)
            .query("per_page", size)
            .query("page", page.number.max(1)),
        cancel,
    )?;
    let items = value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| row(&item_node(item), kind))
        .collect::<Vec<_>>();
    let total = value["total_count"].as_u64();
    Ok(PageOf {
        next: total
            .is_some_and(|total| total > u64::from(page.number.max(1) * size))
            .then_some(Page {
                number: page.number + 1,
                per_page: page.per_page,
            }),
        items,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_pulls_become_graphql_nodes() {
        let pull = json!({
            "node_id": "PR_1", "number": 7, "title": "Fix", "body": "b", "state": "closed",
            "merged_at": "2026-01-02T00:00:00Z", "html_url": "https://github.com/o/r/pull/7",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "user": { "login": "maya" }, "labels": [{ "name": "bug", "color": "f00" }],
            "draft": false, "comments": 2, "mergeable": true, "mergeable_state": "clean",
            "base": { "ref": "main", "sha": "b1", "repo": { "full_name": "o/r" } },
            "head": { "ref": "fix", "sha": "h1", "repo": { "full_name": "fork/r" } },
            "requested_reviewers": [{ "login": "ana" }], "requested_teams": [{ "slug": "core" }],
            "additions": 3, "deletions": 1, "changed_files": 1,
        });
        let contexts = vec![
            check_run_node(&json!({
                "name": "test", "status": "completed", "conclusion": "success",
                "details_url": "https://github.com/o/r/actions/runs/11/job/22",
            })),
            status_node(&json!({ "context": "ci/legacy", "state": "failure" })),
        ];
        let reviews = [json!({
            "node_id": "R_1", "user": { "login": "ana" }, "state": "APPROVED", "body": "",
            "submitted_at": "2026-01-02T00:00:00Z",
        })];
        let commits = [json!({
            "sha": "h1", "commit": { "message": "Fix it\n\nBecause", "committer": { "date": "2026-01-01T00:00:00Z" } },
            "author": { "login": "maya" },
        })];
        let pull = pull_request(&pull_node(&pull, &reviews, &commits, contexts), Vec::new());
        assert_eq!(pull.row.lifecycle, Lifecycle::Merged);
        assert_eq!(pull.row.repo.as_deref(), Some("o/r"));
        assert_eq!(pull.head.repo.as_deref(), Some("fork/r"));
        assert_eq!(pull.mergeable, Mergeable::Clean);
        assert_eq!(pull.reviewers.len(), 2);
        assert_eq!(pull.reviews[0].state, ReviewState::Approved);
        assert_eq!(pull.commits[0].headline, "Fix it");
        assert_eq!(pull.commits[0].body, "Because");
        assert_eq!(pull.checks.len(), 2);
        let run = pull.checks.iter().find(|check| check.name == "test").unwrap();
        assert_eq!(
            run.run_link,
            Some(RunLink {
                run: ItemId::new(11),
                job: Some(ItemId::new(22)),
            })
        );
        assert_eq!(pull.row.ci, Some(CiState::Failed));
        assert_eq!(pull.fingerprint.state, "MERGED");
        assert_eq!(pull.fingerprint.mergeable, "MERGEABLE");
    }

    #[test]
    fn issues_keep_reason_and_repository() {
        let value = json!({
            "node_id": "I_1", "number": 3, "title": "t", "state": "closed",
            "state_reason": "not_planned", "comments": 4,
            "repository_url": "https://api.github.com/repos/o/r",
            "user": { "login": "maya" }, "assignees": [{ "login": "ana" }],
            "milestone": { "number": 1, "title": "v1", "state": "open", "due_on": null },
        });
        let issue = issue(&item_node(&value));
        assert_eq!(issue.row.lifecycle, Lifecycle::ClosedNotPlanned);
        assert_eq!(issue.row.comments, 4);
        assert_eq!(issue.row.repo.as_deref(), Some("o/r"));
        assert_eq!(issue.assignees[0].login, "ana");
        assert_eq!(issue.milestone.unwrap().title, "v1");
    }
}
