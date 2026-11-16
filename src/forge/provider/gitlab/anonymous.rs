//! GitLab without a credential (FORGE.md §2.5). Public projects answer most REST reads
//! anonymously, but labels, milestones, members, notes, discussions and single jobs are 401;
//! GitLab's GraphQL API serves the same data for public projects, so those reads go there and
//! are reshaped into the REST JSON the mappers in `gitlab.rs` read. Job logs, to-dos, the
//! viewer and `@me` filters need a sign-in and answer without a request.

use super::*;

/// GraphQL connections are walked at most this many pages (100 nodes each).
const MAX_CONNECTION_PAGES: usize = 10;

const LABELS_DOCUMENT: &str = "query AnonymousLabels($path: ID!, $after: String) { \
project(fullPath: $path) { labels(first: 100, after: $after, includeAncestorGroups: true) { \
pageInfo { hasNextPage endCursor } nodes { id title color } } } }";

const MILESTONES_DOCUMENT: &str = "query AnonymousMilestones($path: ID!, $after: String) { \
project(fullPath: $path) { milestones(first: 100, after: $after, includeAncestors: true) { \
pageInfo { hasNextPage endCursor } nodes { id iid title dueDate state } } } }";

const MEMBERS_DOCUMENT: &str = "query AnonymousMembers($path: ID!, $after: String) { \
project(fullPath: $path) { projectMembers(first: 100, after: $after) { \
pageInfo { hasNextPage endCursor } nodes { user { id username name } } } } }";

const NOTE_FIELDS: &str = "id body system resolvable resolved createdAt author { id username name } \
position { newPath oldPath newLine oldLine diffRefs { headSha } }";

fn notes_document(kind: ItemKind) -> String {
    let (name, field) = match kind {
        ItemKind::PullRequest => ("AnonymousMergeRequestNotes", "mergeRequest"),
        ItemKind::Issue => ("AnonymousIssueNotes", "issue"),
    };
    format!(
        "query {name}($path: ID!, $iid: String!, $after: String) {{ project(fullPath: $path) {{ \
{field}(iid: $iid) {{ notes(first: 100, after: $after) {{ pageInfo {{ hasNextPage endCursor }} \
nodes {{ {NOTE_FIELDS} }} }} }} }} }}"
    )
}

fn discussions_document() -> String {
    format!(
        "query AnonymousDiscussions($path: ID!, $iid: String!, $after: String) {{ \
project(fullPath: $path) {{ mergeRequest(iid: $iid) {{ discussions(first: 100, after: $after) {{ \
pageInfo {{ hasNextPage endCursor }} nodes {{ id notes {{ nodes {{ {NOTE_FIELDS} }} }} }} }} }} }} }}"
    )
}

const JOB_DOCUMENT: &str = "query AnonymousJob($path: ID!, $id: JobID!) { project(fullPath: $path) { \
job(id: $id) { id name status stage { name } startedAt finishedAt webPath allowFailure retried } } }";

/// The anonymous answer to `read`, or `None` when the REST path works without a credential.
pub(super) fn fetch(
    ctx: &ForgeContext,
    read: &Read,
    cancel: &AtomicBool,
) -> Option<Result<Payload, ForgeError>> {
    Some(match read {
        Read::Viewer => Err(ctx.sign_in_to("see your account")),
        Read::Notifications { .. } => Err(ctx.sign_in_to("see to-dos")),
        Read::JobLog(_) => Err(ctx.sign_in_to("read logs")),
        Read::List { filter, .. } | Read::Search { filter, .. }
            if Query::parse(&filter.query).mentions_me() =>
        {
            Err(ctx.sign_in_to("filter by @me"))
        }
        Read::Labels => connection(ctx, LABELS_DOCUMENT, json!({}), &["labels"], cancel)
            .map(|nodes| {
                Payload::Labels(labels(&Value::Array(
                    nodes
                        .iter()
                        .map(|label| json!({ "title": label["title"], "color": label["color"] }))
                        .collect(),
                )))
            }),
        Read::Milestones => connection(ctx, MILESTONES_DOCUMENT, json!({}), &["milestones"], cancel)
            .map(|nodes| {
                Payload::Milestones(
                    nodes
                        .iter()
                        .filter_map(|node| {
                            milestone(&json!({
                                "id": gid(&node["id"]),
                                "iid": node["iid"],
                                "title": node["title"],
                                "due_date": node["dueDate"],
                                "state": node["state"],
                            }))
                        })
                        .collect(),
                )
            }),
        Read::Collaborators => {
            connection(ctx, MEMBERS_DOCUMENT, json!({}), &["projectMembers"], cancel).map(
                |nodes| {
                    Payload::Users(
                        nodes
                            .iter()
                            .map(|node| &node["user"])
                            .filter(|member| member.is_object())
                            .map(|member| {
                                user(&json!({
                                    "id": gid(&member["id"]),
                                    "username": member["username"],
                                    "name": member["name"],
                                }))
                            })
                            .collect(),
                    )
                },
            )
        }
        Read::Timeline(number) => {
            let kind = recall(ctx, *number);
            let field = match kind {
                ItemKind::PullRequest => "mergeRequest",
                ItemKind::Issue => "issue",
            };
            connection(
                ctx,
                &notes_document(kind),
                json!({ "iid": number.to_string() }),
                &[field, "notes"],
                cancel,
            )
            .map(|nodes| {
                Payload::Timeline(
                    nodes
                        .iter()
                        .map(note)
                        .flat_map(|note| timeline_events(&note))
                        .collect(),
                )
            })
        }
        Read::Threads(number) => {
            remember(ctx, *number, ItemKind::PullRequest);
            connection(
                ctx,
                &discussions_document(),
                json!({ "iid": number.to_string() }),
                &["mergeRequest", "discussions"],
                cancel,
            )
            .map(|nodes| {
                Payload::Threads(
                    nodes
                        .iter()
                        .filter_map(|discussion| thread(*number, &discussion_value(discussion)))
                        .collect(),
                )
            })
        }
        Read::Job(id) => job(ctx, id, cancel).map(|job| Payload::Job(Box::new(job))),
        _ => return None,
    })
}

/// `gid://gitlab/Note/42` → 42 (REST ids are numbers); other ids pass through.
fn gid(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|id| id.rsplit('/').next())
        .map(|tail| match tail.parse::<u64>() {
            Ok(number) => Value::from(number),
            Err(_) => Value::from(tail),
        })
        .unwrap_or(Value::Null)
}

/// A GraphQL note as the REST note `note()`, `thread()` and `timeline_events()` read.
fn note(node: &Value) -> Value {
    let position = &node["position"];
    let rest_position = position.is_object().then(|| {
        json!({
            "new_path": position["newPath"],
            "old_path": position["oldPath"],
            "new_line": position["newLine"],
            "old_line": position["oldLine"],
            "head_sha": position["diffRefs"]["headSha"],
        })
    });
    json!({
        "id": gid(&node["id"]),
        "body": node["body"],
        "system": node["system"],
        "resolvable": node["resolvable"],
        "resolved": node["resolved"],
        "created_at": node["createdAt"],
        "author": {
            "id": gid(&node["author"]["id"]),
            "username": node["author"]["username"],
            "name": node["author"]["name"],
        },
        "type": if rest_position.is_some() { Value::from("DiffNote") } else { Value::Null },
        "position": rest_position,
    })
}

fn discussion_value(node: &Value) -> Value {
    json!({
        // REST discussion ids are the hex tail of the GraphQL id.
        "id": node["id"].as_str().and_then(|id| id.rsplit('/').next()).unwrap_or_default(),
        "notes": nodes(&node["notes"]).iter().map(note).collect::<Vec<_>>(),
    })
}

/// Every page of `project.<path>` (bounded).
fn connection(
    ctx: &ForgeContext,
    document: &str,
    variables: Value,
    path: &[&str],
    cancel: &AtomicBool,
) -> Result<Vec<Value>, ForgeError> {
    let full = ctx.repo()?.full();
    let mut variables = variables;
    variables["path"] = full.clone().into();
    let mut all = Vec::new();
    for _ in 0..MAX_CONNECTION_PAGES {
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        let data = GitLab::graphql(ctx, document, variables.clone(), cancel)?;
        let mut at = &data["project"];
        if !at.is_object() {
            return Err(ForgeError::NotFound(format!("project {full}")));
        }
        for key in path {
            at = &at[*key];
        }
        if at.is_null() {
            return Err(ForgeError::NotFound(format!("{} in {full}", path.join("."))));
        }
        all.extend(nodes(at).iter().cloned());
        match at["pageInfo"]["endCursor"].as_str() {
            Some(cursor) if at["pageInfo"]["hasNextPage"].as_bool() == Some(true) => {
                variables["after"] = cursor.into();
            }
            _ => break,
        }
    }
    Ok(all)
}

/// `jobs/{id}` is 401 without a credential; GraphQL's `project.job` is not.
fn job(ctx: &ForgeContext, id: &ItemId, cancel: &AtomicBool) -> Result<Job, ForgeError> {
    let full = ctx.repo()?.full();
    let data = GitLab::graphql(
        ctx,
        JOB_DOCUMENT,
        json!({ "path": full, "id": format!("gid://gitlab/Ci::Build/{id}") }),
        cancel,
    )?;
    let node = &data["project"]["job"];
    if !node.is_object() {
        return Err(ForgeError::NotFound(format!("job {id}")));
    }
    Ok(pipeline_job(&json!({
        "id": gid(&node["id"]),
        "name": node["name"],
        "status": node["status"].as_str().map(str::to_ascii_lowercase),
        "stage": node["stage"]["name"],
        "started_at": node["startedAt"],
        "finished_at": node["finishedAt"],
        "web_url": node["webPath"]
            .as_str()
            .map(|path| format!("{}{path}", ctx.endpoints.web_base)),
        "allow_failure": node["allowFailure"],
        "retried": node["retried"],
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_notes_read_like_rest_notes() {
        let node = json!({
            "id": "gid://gitlab/Note/42", "body": "nit", "system": false,
            "resolvable": true, "resolved": false, "createdAt": "2026-01-01T00:00:00Z",
            "author": { "id": "gid://gitlab/User/7", "username": "maya", "name": "Maya" },
            "position": { "newPath": "a.rs", "oldPath": "a.rs", "newLine": 3, "oldLine": null,
                "diffRefs": { "headSha": "abc" } },
        });
        let rest = note(&node);
        assert_eq!(rest["id"], 42);
        assert_eq!(rest["type"], "DiffNote");
        let discussion = discussion_value(&json!({
            "id": "gid://gitlab/Discussion/6a9c",
            "notes": { "nodes": [node] },
        }));
        let thread = thread(5, &discussion).unwrap();
        assert_eq!(thread.id, ItemId::new("5/6a9c"));
        assert_eq!(thread.path.as_deref(), Some("a.rs"));
        assert_eq!(thread.resolved, Some(false));
        assert_eq!(thread.comments[0].author.login, "maya");
        // Diff notes stay out of the conversation, like REST's.
        assert!(timeline_events(&rest).is_empty());
    }
}
