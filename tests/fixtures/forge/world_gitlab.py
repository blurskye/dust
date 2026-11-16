"""A stateful GitLab for the fake forge (`server.py --world gitlab`).

One project, `orbit/team/orbit` (a nested group, id 6329679), with merge requests, issues,
discussions, approvals, pipelines and To-dos, answering the REST v4 and GraphQL calls the GitLab
provider makes. Shapes follow what gitlab.com answered when recorded (VERSIONS.md). Writes change the world,
so a test can read its own write back. `state["gitlab"]` holds the world once seeded; a test may
seed it itself. REST lists page with `x-next-page` / `x-total` like GitLab.
"""
import copy
import json
import io
import re
import time
import urllib.parse
import zipfile

PROJECT = "orbit/team/orbit"
PROJECT_ID = 6329679
WEB = "https://gitlab.com/" + PROJECT
HEAD = "c92ad11000000000000000000000000000000000"
BASE = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

USERS = {
    "rowan": {"id": 1, "username": "rowan", "name": "Rowan Reyes"},
    "maya": {"id": 1001, "username": "maya", "name": "Maya Ito"},
    "alex": {"id": 1002, "username": "alex", "name": "Alex Kim"},
    "sam": {"id": 1003, "username": "sam", "name": "Sam Rao"},
}
LABELS = [
    {"id": 1, "name": "backend", "color": "#D10069"},
    {"id": 2, "name": "bug", "color": "#DC143C"},
    {"id": 3, "name": "pipeline", "color": "#428BCA"},
    {"id": 4, "name": "wip", "color": "#F0AD4E"},
]
MILESTONES = [{"id": 12, "iid": 3, "title": "v1.4", "due_date": "2026-12-01", "state": "active"}]

DIFF_13 = [
    {"old_path": "src/lib.rs", "new_path": "src/lib.rs", "a_mode": "100644", "b_mode": "100644",
     "new_file": False, "renamed_file": False, "deleted_file": False,
     "diff": "@@ -10,4 +10,6 @@ pub fn upload(job: &Job) -> Result<()> {\n     let client = Client::new();\n"
             "-    client.put(job.path())\n+    retry(3, || client.put(job.path()))\n"
             "+        .context(\"upload failed\")?;\n+    Ok(())\n }\n"},
    {"old_path": "ci/retry.rs", "new_path": "ci/retry.rs", "a_mode": "0", "b_mode": "100644",
     "new_file": True, "renamed_file": False, "deleted_file": False,
     "diff": "@@ -0,0 +1,3 @@\n+pub fn retry<T>(times: u32, f: impl Fn() -> T) -> T {\n+    f()\n+}\n"},
]

TRACE = ("\x1b[0KRunning with gitlab-runner 18.5.0\n"
         "\x1b[0Ksection_start:1763100000:prepare_script\r\x1b[0KPreparing environment\n"
         "Running on runner-1 via fake...\n"
         "\x1b[0Ksection_end:1763100004:prepare_script\r\x1b[0K\n"
         "\x1b[0Ksection_start:1763100004:step_script\r\x1b[0K\x1b[32;1m$ cargo test\x1b[0;m\n"
         "running 12 tests\ntest upload::retries ... FAILED\n"
         "test result: FAILED. 11 passed; 1 failed\n"
         "\x1b[0Ksection_end:1763100300:step_script\r\x1b[0K\n"
         "\x1b[31;1mERROR: Job failed: exit code 101\x1b[0;m\n")


def running_trace():
    """Job 20's trace so far: `$ cargo test` opened under a minute after the job started and has
    not ended, so the step reads `running 3m` on the server clock."""
    begun = int(now()) - 3 * 60 - 50
    return ("\x1b[0KRunning with gitlab-runner 18.5.0\n"
            f"\x1b[0Ksection_start:{begun}:prepare_script\r\x1b[0KPreparing environment\n"
            "Running on runner-1 via fake...\n"
            f"\x1b[0Ksection_end:{begun + 4}:prepare_script\r\x1b[0K\n"
            f"\x1b[0Ksection_start:{begun + 4}:step_script\r\x1b[0K\x1b[32;1m$ cargo test\x1b[0;m\n"
            "running 12 tests\n")


# F6: job 44's artifact archive (`jobs/44/artifacts`, `jobs/44/artifacts/<path>`).
ARTIFACT_FILES = {"target/release/orbit": b"\x7fELF fake binary\n",
                  "coverage/lcov.info": b"TN:\nSF:src/lib.rs\nend_of_record\n"}


def artifact_zip(files=None):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for path, contents in (files or ARTIFACT_FILES).items():
            info = zipfile.ZipInfo(path, date_time=(2026, 11, 14, 5, 1, 35))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, contents)
    return buffer.getvalue()


ARTIFACT_SIZE = len(artifact_zip())


def user(name):
    return dict(USERS[name], state="active", web_url="https://gitlab.com/" + name)


def note(note_id, author, body, at, system=False, kind=None, position=None, resolvable=False):
    return {"id": note_id, "type": kind, "body": body, "author": user(author), "created_at": at,
            "updated_at": at, "system": system, "resolvable": resolvable, "resolved": False,
            "position": position}


def position(path, line):
    return {"position_type": "text", "base_sha": BASE, "start_sha": BASE, "head_sha": HEAD,
            "old_path": path, "new_path": path, "old_line": None, "new_line": line}


def merge_request(iid, title, author, state, updated, **extra):
    request = {
        "id": 400000 + iid, "iid": iid, "project_id": PROJECT_ID, "title": title,
        "description": extra.pop("description", ""), "state": state, "draft": False,
        "created_at": "2026-11-01T09:00:00Z", "updated_at": updated, "author": user(author),
        "assignees": [], "reviewers": [], "labels": [], "milestone": None,
        "source_branch": f"topic-{iid}", "target_branch": "main",
        "source_project_id": PROJECT_ID, "target_project_id": PROJECT_ID,
        "sha": f"{iid:02d}" + HEAD[2:], "detailed_merge_status": "mergeable", "has_conflicts": False,
        "user_notes_count": 0, "subscribed": True, "head_pipeline": None,
        "diff_refs": {"base_sha": BASE, "head_sha": f"{iid:02d}" + HEAD[2:], "start_sha": BASE},
        "references": {"full": f"{PROJECT}!{iid}", "short": f"!{iid}"},
        "web_url": f"{WEB}/-/merge_requests/{iid}",
        "approvals_required": 1, "approved_by": [], "reviewer_states": {}, "diffs": [], "commits": [],
        "discussions": [], "notes": [], "closes": [], "drafts": [],
    }
    request.update(extra)
    return request


def issue(iid, title, author, state, updated, **extra):
    item = {
        "id": 500000 + iid, "iid": iid, "project_id": PROJECT_ID, "title": title,
        "description": extra.pop("description", ""), "state": state,
        "created_at": "2026-11-02T09:00:00Z", "updated_at": updated, "author": user(author),
        "assignees": [], "labels": [], "milestone": None, "user_notes_count": 0, "subscribed": False,
        "references": {"full": f"{PROJECT}#{iid}", "short": f"#{iid}"},
        "web_url": f"{WEB}/-/issues/{iid}", "notes": [], "closed_by": [],
    }
    item.update(extra)
    return item


# A test that compares two servers' answers (`gitlab_transports_agree`) pins the clock with
# `state["clock"]` (epoch seconds), so both date their running work and writes alike.
CLOCK = [None]


def now():
    return CLOCK[0] if CLOCK[0] is not None else time.time()


def stamp(seconds_ago=0):
    """The server clock, `seconds_ago` back: running work and fresh writes are dated from it, so
    a running job reads `running 3m`, not `running 55h`, however long after the fixture was written."""
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now() - seconds_ago))


def pipeline(pipeline_id, iid, ref, status, sha, source, created, jobs):
    return {"id": pipeline_id, "iid": iid, "project_id": PROJECT_ID, "sha": sha, "ref": ref,
            "status": status, "source": source, "created_at": created, "updated_at": created,
            "duration": 312, "web_url": f"{WEB}/-/pipelines/{pipeline_id}", "name": None,
            "user": user("maya"), "jobs": jobs}


def job(job_id, name, stage, status, allow_failure=False, started="2026-11-14T05:00:00Z", duration=95.0):
    return {"id": job_id, "name": name, "stage": stage, "status": status, "allow_failure": allow_failure,
            "started_at": started if status not in ("created", "manual", "pending") else None,
            "finished_at": "2026-11-14T05:01:35Z" if status in ("success", "failed", "canceled") else None,
            "duration": duration if status in ("success", "failed", "canceled") else None,
            "web_url": f"{WEB}/-/jobs/{job_id}", "retried": False,
            "failure_reason": "script_failure" if status == "failed" else None}


def seed():
    pipelines = [
        pipeline(91, 207, "refs/merge-requests/13/head", "failed", "13" + HEAD[2:], "merge_request_event",
                 "2026-11-14T05:00:00Z",
                 [job(42, "test", "test", "failed"), job(43, "clippy", "test", "success"),
                  dict(job(44, "build", "build", "success"),
                       artifacts_file={"filename": "artifacts.zip", "size": ARTIFACT_SIZE},
                       artifacts=[{"file_type": "archive", "size": ARTIFACT_SIZE, "filename": "artifacts.zip",
                                   "file_format": "zip"},
                                  {"file_type": "trace", "size": 1200, "filename": "job.log", "file_format": None}],
                       artifacts_expire_at="2026-12-14T05:01:35Z"),
                  job(45, "deploy", "deploy", "manual", allow_failure=True)]),
        pipeline(90, 206, "main", "success", BASE, "push", "2026-11-13T18:20:00Z",
                 [job(30, "test", "test", "success"), job(31, "build", "build", "success")]),
        pipeline(89, 205, "topic-11", "running", "11" + HEAD[2:], "push", stamp(4 * 60),
                 [job(20, "test", "test", "running", started=stamp(3 * 60 + 50)),
                  job(21, "build", "build", "created")]),
    ]
    requests = [
        merge_request(
            13, "Retry flaky artifact uploads", "maya", "opened", "2026-11-14T05:30:00Z",
            description="Uploads to the artifact store fail about once a day.\n\nThis retries them three times.",
            reviewers=[user("alex"), user("sam")], assignees=[user("maya")],
            labels=["backend", "pipeline"], milestone=MILESTONES[0], user_notes_count=4,
            detailed_merge_status="not_approved",
            head_pipeline={k: v for k, v in pipelines[0].items() if k != "jobs"},
            source_branch="retry-uploads", sha=HEAD,
            diff_refs={"base_sha": BASE, "head_sha": HEAD, "start_sha": BASE},
            approved_by=[], reviewer_states={"alex": "requested_changes", "sam": "unreviewed"},
            diffs=DIFF_13,
            commits=[{"id": HEAD, "short_id": HEAD[:8], "title": "Retry artifact uploads three times",
                      "message": "Retry artifact uploads three times\n\nFlaky store.", "author_name": "Maya Ito",
                      "created_at": "2026-11-14T04:58:00Z", "committed_date": "2026-11-14T04:58:00Z"}],
            discussions=[
                {"id": "6a9c1750b37d513a43987b574953fceb50b03ce7", "individual_note": False, "notes": [
                    note(601, "alex", "nit: name the retry count", "2026-11-14T05:26:33Z", kind="DiffNote",
                         position=position("src/lib.rs", 12), resolvable=True),
                    note(602, "maya", "Done.", "2026-11-14T05:28:00Z", kind="DiffNote",
                         position=position("src/lib.rs", 12), resolvable=True)]},
                {"id": "77d0c0ffee", "individual_note": False, "notes": [
                    note(603, "sam", "Should this back off between tries?", "2026-11-14T05:29:00Z",
                         kind="DiffNote", position=position("ci/retry.rs", 2), resolvable=True)]},
            ],
            notes=[
                note(701, "maya", "Ready for review.", "2026-11-14T05:00:30Z"),
                note(702, "alex", "requested changes", "2026-11-14T05:27:00Z", system=True),
                note(703, "sam", "added ~pipeline label", "2026-11-14T05:29:30Z", system=True),
                note(704, "alex", "Looks close; one naming nit.", "2026-11-14T05:30:00Z"),
            ],
            closes=[14]),
        merge_request(12, "Cache cargo registry between pipeline stages", "alex", "merged", "2026-11-13T18:00:00Z",
                      labels=["pipeline"], approved_by=["sam"], detailed_merge_status="not_open"),
        merge_request(11, "Draft: Split nested group settings", "sam", "opened", "2026-11-13T16:00:00Z",
                      draft=True, detailed_merge_status="draft_status", labels=["wip"],
                      head_pipeline={k: v for k, v in pipelines[2].items() if k != "jobs"}),
        merge_request(10, "Document merge request approvals", "maya", "opened", "2026-11-12T10:00:00Z",
                      approved_by=["alex"], reviewers=[user("alex")], reviewer_states={"alex": "approved"},
                      head_pipeline={k: v for k, v in pipelines[1].items() if k != "jobs"}),
    ]
    issues = [
        issue(14, "Artifact uploads fail on nested groups", "sam", "opened", "2026-11-14T04:00:00Z",
              description="`orbit/team/orbit` uploads 404 about once a day.", labels=["bug", "pipeline"],
              assignees=[user("maya")], milestone=MILESTONES[0], user_notes_count=1,
              notes=[note(801, "maya", "Fixed by !13 once it lands.", "2026-11-14T05:01:00Z")], closed_by=[13]),
        issue(9, "Pipelines tab shows stale status", "alex", "opened", "2026-11-11T12:00:00Z", labels=["bug"]),
        issue(7, "Rename the default branch", "maya", "closed", "2026-11-05T12:00:00Z"),
    ]
    todos = [
        {"id": 9001, "action_name": "review_requested", "target_type": "MergeRequest", "target_iid": 13,
         "author": "maya", "state": "pending", "created_at": "2026-11-14T05:00:40Z", "body": "Retry flaky artifact uploads"},
        {"id": 9002, "action_name": "mentioned", "target_type": "Issue", "target_iid": 14,
         "author": "sam", "state": "pending", "created_at": "2026-11-14T04:00:00Z", "body": "@rowan can you look?"},
        {"id": 9003, "action_name": "build_failed", "target_type": "MergeRequest", "target_iid": 13,
         "author": "maya", "state": "done", "created_at": "2026-11-14T05:02:00Z", "body": "Pipeline failed"},
    ]
    return {"merge_requests": requests, "issues": issues, "pipelines": pipelines, "todos": todos,
            "next_id": 10000, "graphql_mutations": ["mergeRequestRequestChanges", "mergeRequestAccept"],
            "draft_notes": True, "traces": {"20": running_trace()}}


class Reply(Exception):
    def __init__(self, status, body, headers=None):
        super().__init__(status)
        self.status, self.body, self.headers = status, body, headers or {}


def fail(status, message):
    raise Reply(status, {"message": message})


# --------------------------------------------------------------------------- REST shapes


def public(item, hidden=("approved_by", "reviewer_states", "diffs", "commits", "discussions", "notes",
                         "closes", "drafts", "closed_by", "approvals_required", "jobs",
                         "resource_label_events", "resource_milestone_events", "resource_state_events",
                         "award_emoji")):
    return {key: value for key, value in item.items() if key not in hidden}


def page(items, query):
    per_page = max(1, min(100, int(query.get("per_page") or 20)))
    number = max(1, int(query.get("page") or 1))
    chunk = items[(number - 1) * per_page: number * per_page]
    more = number * per_page < len(items)
    headers = {"x-page": str(number), "x-per-page": str(per_page), "x-total": str(len(items)),
               "x-total-pages": str(max(1, -(-len(items) // per_page))),
               "x-next-page": str(number + 1) if more else ""}
    raise Reply(200, chunk, headers)


def find(items, key, value):
    for item in items:
        if item[key] == value:
            return item
    fail(404, "404 Not found")


def labels_of(item):
    return [label for label in LABELS if label["name"] in item["labels"]]


def matches(item, query, _viewer):
    state = query.get("state")
    if state and state != "all" and item["state"] != state:
        return False
    if query.get("author_username") and item["author"]["username"] != query["author_username"]:
        return False
    if query.get("assignee_username") and query["assignee_username"] not in [a["username"] for a in item["assignees"]]:
        return False
    if query.get("reviewer_username") and query["reviewer_username"] not in [
            r["username"] for r in item.get("reviewers", [])]:
        return False
    for label in filter(None, (query.get("labels") or "").split(",")):
        if label not in item["labels"]:
            return False
    for label in filter(None, (query.get("not[labels]") or "").split(",")):
        if label in item["labels"]:
            return False
    if query.get("source_branch") and item.get("source_branch") != query["source_branch"]:
        return False
    if query.get("search") and query["search"].lower() not in (item["title"] + item["description"]).lower():
        return False
    if query.get("wip") == "yes" and not item.get("draft"):
        return False
    if query.get("milestone") and (item.get("milestone") or {}).get("title") != query["milestone"]:
        return False
    return True


def sorted_items(items):
    return sorted(items, key=lambda item: item["updated_at"], reverse=True)


def mr_detail(world, item):
    body = public(item)
    body["labels"] = list(item["labels"])
    body["head_pipeline"] = copy.deepcopy(item["head_pipeline"])
    return body


def mr_approvals(item):
    approved = [{"user": user(name), "approved_at": "2026-11-14T05:10:00Z"} for name in item["approved_by"]]
    left = max(0, item["approvals_required"] - len(approved))
    return {"id": item["id"], "iid": item["iid"], "approved": left == 0, "approvals_required": item["approvals_required"],
            "approvals_left": left, "approved_by": approved}


def mr_reviewers(item):
    return [{"user": user(r["username"]), "state": item["reviewer_states"].get(r["username"], "unreviewed"),
             "created_at": "2026-11-14T05:00:00Z"} for r in item["reviewers"]]


def todo_body(world, todo):
    if todo["target_type"] == "MergeRequest":
        target = public(find(world["merge_requests"], "iid", todo["target_iid"]))
    else:
        target = public(find(world["issues"], "iid", todo["target_iid"]))
    return {"id": todo["id"], "project": {"id": PROJECT_ID, "path_with_namespace": PROJECT},
            "author": user(todo["author"]), "action_name": todo["action_name"], "target_type": todo["target_type"],
            "target": target, "target_url": target["web_url"], "body": todo["body"], "state": todo["state"],
            "created_at": todo["created_at"], "updated_at": todo["created_at"]}


def project_body():
    return {"id": PROJECT_ID, "path_with_namespace": PROJECT, "name": "orbit", "default_branch": "main",
            "web_url": WEB, "merge_method": "merge", "squash_option": "default_off",
            "remove_source_branch_after_merge": True, "jobs_enabled": True, "builds_access_level": "enabled",
            "permissions": {"project_access": {"access_level": 40}, "group_access": None}}


def ids_to_users(ids):
    by_id = {u["id"]: name for name, u in USERS.items()}
    return [user(by_id[i]) for i in ids if i in by_id]


# --------------------------------------------------------------------------- GraphQL


def graphql(world, request, viewer):
    document = request.document
    variables = request.variables or {}
    operation = request.operation
    if operation == "MutationFields":
        return {"data": {"__type": {"fields": [{"name": name} for name in world["graphql_mutations"]]}}}
    if operation == "MergeRequestRequestChanges":
        if "mergeRequestRequestChanges" not in world["graphql_mutations"]:
            return {"data": None, "errors": [{"message": "Field 'mergeRequestRequestChanges' doesn't exist on type 'Mutation'"}]}
        item = find(world["merge_requests"], "iid", int(variables["iid"]))
        item["reviewer_states"][viewer] = "requested_changes"
        if viewer in USERS and viewer not in [r["username"] for r in item["reviewers"]]:
            item["reviewers"].append(user(viewer))
        if viewer in item["approved_by"]:
            item["approved_by"].remove(viewer)
        return {"data": {"mergeRequestRequestChanges": {"errors": [], "mergeRequest": {"iid": str(item["iid"])}}}}
    if operation in ("MergeRequestList", "IssueList"):
        if variables.get("path") != PROJECT:
            return {"data": {"project": None}, "errors": [{"message": "The resource that you are attempting to access does not exist or you don't have permission to perform this action"}]}
        mrs = operation == "MergeRequestList"
        items = world["merge_requests"] if mrs else world["issues"]
        query = {"state": variables.get("state"), "author_username": variables.get("authorUsername"),
                 "assignee_username": variables.get("assigneeUsername"),
                 "reviewer_username": variables.get("reviewerUsername"),
                 "labels": ",".join(variables.get("labels") or variables.get("labelName") or []),
                 "not[labels]": ",".join(((variables.get("not") or {}).get("labels")
                                          or (variables.get("not") or {}).get("labelName") or [])),
                 "search": variables.get("search"), "wip": "yes" if variables.get("draft") else None,
                 "milestone": (variables["milestoneTitle"] if isinstance(variables.get("milestoneTitle"), str)
                               else (variables.get("milestoneTitle") or [None])[0])}
        chosen = [item for item in sorted_items(items) if matches(item, query, viewer)]
        if variables.get("approvedBy"):
            chosen = [item for item in chosen if set(variables["approvedBy"]) & set(item["approved_by"])]
        start = int(variables.get("after") or 0)
        first = int(variables.get("first") or 20)
        window = chosen[start:start + first]
        nodes = []
        for item in window:
            node = {"iid": str(item["iid"]), "id": f"gid://gitlab/{'MergeRequest' if mrs else 'Issue'}/{item['id']}",
                    "title": item["title"], "description": item["description"], "state": item["state"],
                    "webUrl": item["web_url"], "updatedAt": item["updated_at"], "userNotesCount": item["user_notes_count"],
                    "author": {"username": item["author"]["username"], "name": item["author"]["name"]},
                    "labels": {"nodes": [{"title": l["name"], "color": l["color"]} for l in labels_of(item)]}}
            if mrs:
                approvals = mr_approvals(item)
                stats = [0, 0]
                for diff in item["diffs"]:
                    for line in diff["diff"].splitlines():
                        if line.startswith("+"):
                            stats[0] += 1
                        elif line.startswith("-"):
                            stats[1] += 1
                pipe = item["head_pipeline"]
                node.update({
                    "draft": item["draft"], "mergeStatusEnum": "CAN_BE_MERGED", "approved": approvals["approved"],
                    "sourceBranch": item["source_branch"],
                    "approvedBy": {"nodes": [{"username": n} for n in item["approved_by"]]},
                    "reviewers": {"nodes": [{"username": r["username"]} for r in item["reviewers"]]},
                    "headPipeline": {"status": pipe["status"].upper()} if pipe else None,
                    "diffStatsSummary": {"additions": stats[0], "deletions": stats[1], "fileCount": len(item["diffs"])},
                })
            nodes.append(node)
        end = start + len(window)
        connection = {"count": len(chosen), "pageInfo": {"hasNextPage": end < len(chosen), "endCursor": str(end)},
                      "nodes": nodes}
        assert ("mergeRequests" if mrs else "issues") in document
        return {"data": {"project": {"mergeRequests" if mrs else "issues": connection}}}
    if operation in ANONYMOUS_OPERATIONS:
        if variables.get("path") != PROJECT:
            return {"data": {"project": None}}
        return {"data": {"project": anonymous_project(world, operation, variables)}}
    return {"data": None, "errors": [{"message": f"fake gitlab: unknown operation {operation}"}]}


# --------------------------------------------------------------------------- F7: anonymous GraphQL

# Public projects answer these over GraphQL without a sign-in, where REST is 401.
ANONYMOUS_OPERATIONS = ("AnonymousLabels", "AnonymousMilestones", "AnonymousMembers", "AnonymousMergeRequestNotes",
                        "AnonymousIssueNotes", "AnonymousDiscussions", "AnonymousJob")


def gql_user(name_or_user):
    found = USERS.get(name_or_user) if isinstance(name_or_user, str) else name_or_user
    return {"id": f"gid://gitlab/User/{found['id']}", "username": found["username"], "name": found["name"]}


def gql_note(each):
    pos = each.get("position")
    return {"id": f"gid://gitlab/Note/{each['id']}", "body": each["body"], "system": each.get("system", False),
            "resolvable": each.get("resolvable", False), "resolved": each.get("resolved", False),
            "createdAt": each["created_at"], "author": gql_user(each["author"]),
            "position": {"newPath": pos["new_path"], "oldPath": pos["old_path"], "newLine": pos.get("new_line"),
                         "oldLine": pos.get("old_line"), "diffRefs": {"headSha": pos.get("head_sha")}} if pos else None}


def gql_connection(nodes, variables):
    start = int(variables.get("after") or 0)
    window = nodes[start:start + 100]
    end = start + len(window)
    return {"pageInfo": {"hasNextPage": end < len(nodes), "endCursor": str(end)}, "nodes": window}


def anonymous_project(world, operation, variables):
    if operation == "AnonymousLabels":
        return {"labels": gql_connection([{"id": f"gid://gitlab/ProjectLabel/{l['id']}", "title": l["name"],
                                           "color": l["color"]} for l in LABELS], variables)}
    if operation == "AnonymousMilestones":
        return {"milestones": gql_connection([{"id": f"gid://gitlab/Milestone/{m['id']}", "iid": str(m["iid"]),
                                               "title": m["title"], "dueDate": m["due_date"], "state": m["state"]}
                                              for m in MILESTONES], variables)}
    if operation == "AnonymousMembers":
        return {"projectMembers": gql_connection([{"user": gql_user(name)} for name in ("maya", "alex", "sam", "rowan")],
                                                 variables)}
    if operation == "AnonymousJob":
        job_id = int(str(variables.get("id", "")).rsplit("/", 1)[-1])
        found = next((j for p in world["pipelines"] for j in p["jobs"] if j["id"] == job_id), None)
        if not found:
            return {"job": None}
        return {"job": {"id": f"gid://gitlab/Ci::Build/{job_id}", "name": found["name"],
                        "status": str(found["status"]).upper(), "stage": {"name": found.get("stage")},
                        "startedAt": found.get("started_at"), "finishedAt": found.get("finished_at"),
                        "webPath": "/" + PROJECT + f"/-/jobs/{job_id}", "allowFailure": found.get("allow_failure", False),
                        "retried": found.get("retried", False)}}
    mrs = operation in ("AnonymousMergeRequestNotes", "AnonymousDiscussions")
    items = world["merge_requests"] if mrs else world["issues"]
    item = next((i for i in items if str(i["iid"]) == str(variables.get("iid"))), None)
    field = "mergeRequest" if mrs else "issue"
    if item is None:
        return {field: None}
    if operation == "AnonymousDiscussions":
        discussions = [{"id": f"gid://gitlab/Discussion/{d['id']}", "notes": {"nodes": [gql_note(n) for n in d["notes"]]}}
                       for d in item["discussions"]]
        return {field: {"discussions": gql_connection(discussions, variables)}}
    notes = list(item["notes"])
    return {field: {"notes": gql_connection([gql_note(n) for n in notes], variables)}}


# --------------------------------------------------------------------------- dispatch


def answer(state, request):
    CLOCK[0] = state.get("clock")
    world = state.get("gitlab")
    if world is None:
        world = state["gitlab"] = seed()
        if state.get("actions_seed") == "design":
            import world_actions
            world["pipelines"], world["traces"] = world_actions.gitlab_world()
            world["schedules"] = world_actions.GITLAB_SCHEDULES
    if state.get("conversation"):
        import sys
        import world_conversation
        world_conversation.gitlab(state, world, sys.modules[__name__])
    viewer = state.get("viewer", "rowan")
    if request.graphql:
        return 200, graphql(world, request, viewer)
    return rest(world, request, viewer)


def rest(world, request, viewer):
    method, query, body = request.method, request.query, request.json or {}
    path = request.rest.split("?", 1)[0]
    if method == "GET" and path == "user":
        return 200, user(viewer if viewer in USERS else "rowan")
    if method == "GET" and path == "users":
        name = query.get("username", "")
        return 200, [user(name)] if name in USERS else []
    if path == "todos":
        todos = [todo_body(world, t) for t in world["todos"] if query.get("state") in (None, t["state"])]
        page(todos, query)
    match = re.fullmatch(r"todos/(\d+)/mark_as_done", path)
    if match and method == "POST":
        todo = find(world["todos"], "id", int(match.group(1)))
        todo["state"] = "done"
        return 200, todo_body(world, todo)
    if path == "todos/mark_as_done" and method == "POST":
        for todo in world["todos"]:
            todo["state"] = "done"
        return 204, None
    if path in ("merge_requests", "issues"):
        items = world["merge_requests"] if path == "merge_requests" else world["issues"]
        page([public(i) for i in sorted_items(items) if matches(i, query, viewer)], query)
    match = re.fullmatch(r"(?:projects|groups)/([^/]+)(?:/(.*))?", path)
    if not match:
        fail(404, "404 Not Found")
    project = urllib.parse.unquote(match.group(1))
    tail = match.group(2) or ""
    if project not in (PROJECT, str(PROJECT_ID), "orbit/team", "orbit"):
        fail(404, "404 Project Not Found")
    return project_route(world, method, tail, query, body, viewer)


def project_route(world, method, tail, query, body, viewer):
    mrs, issues = world["merge_requests"], world["issues"]
    if tail == "" and method == "GET":
        return 200, project_body()
    if tail == "labels":
        page(LABELS, query)
    if tail == "milestones":
        chosen = [m for m in MILESTONES if not query.get("title") or m["title"] == query["title"]]
        page(chosen, query)
    if tail == "members/all":
        page([user(name) for name in ("maya", "alex", "sam", "rowan")], query)
    if tail in ("merge_requests", "issues") and method == "GET":
        items = mrs if tail == "merge_requests" else issues
        page([public(i) for i in sorted_items(items) if matches(i, query, viewer)], query)
    if tail == "issues" and method == "POST":
        world["next_id"] += 1
        created = issue(max(i["iid"] for i in issues + mrs) + 1, body.get("title", ""), viewer if viewer in USERS else "rowan",
                        "opened", stamp(), description=body.get("description", ""),
                        labels=[l for l in (body.get("labels") or "").split(",") if l],
                        assignees=ids_to_users(body.get("assignee_ids") or []))
        issues.append(created)
        return 201, public(created)
    if tail == "merge_requests" and method == "POST":
        created = merge_request(max(i["iid"] for i in issues + mrs) + 1, body.get("title", ""),
                                viewer if viewer in USERS else "rowan", "opened", stamp(),
                                description=body.get("description", ""), source_branch=body.get("source_branch"),
                                target_branch=body.get("target_branch"),
                                draft=body.get("title", "").lower().startswith("draft:"))
        mrs.append(created)
        return 201, public(created)
    match = re.fullmatch(r"(merge_requests|issues)/(\d+)(?:/(.*))?", tail)
    if match:
        kind, iid, rest = match.group(1), int(match.group(2)), match.group(3) or ""
        item = find(mrs if kind == "merge_requests" else issues, "iid", iid)
        return item_route(world, kind, item, method, rest, query, body, viewer)
    match = re.fullmatch(r"pipelines/(\d+)(?:/(.*))?", tail)
    if match:
        pipe = find(world["pipelines"], "id", int(match.group(1)))
        rest = match.group(2) or ""
        if rest == "" and method == "GET":
            return 200, public(pipe)
        if rest == "jobs":
            page(pipe["jobs"], query)
        if rest == "bridges":
            page([], query)
        if rest == "retry" and method == "POST":
            for each in pipe["jobs"]:
                if each["status"] in ("failed", "canceled"):
                    each["status"] = "pending"
            pipe["status"] = "running"
            return 201, public(pipe)
        if rest == "cancel" and method == "POST":
            for each in pipe["jobs"]:
                if each["status"] in ("running", "pending", "created"):
                    each["status"] = "canceled"
            pipe["status"] = "canceled"
            return 200, public(pipe)
    if tail == "pipelines":
        chosen = [p for p in world["pipelines"] if (not query.get("ref") or p["ref"] == query["ref"])
                  and (not query.get("source") or p["source"] == query["source"])
                  and (not query.get("status") or p["status"] == query["status"])
                  and (not query.get("updated_after") or p["updated_at"] > query["updated_after"])]
        if query.get("order_by") == "updated_at":
            chosen = sorted(chosen, key=lambda p: p["updated_at"], reverse=True)
        page([public(p) for p in chosen], query)
    if tail == "pipeline_schedules" and method == "GET":
        page(world.get("schedules", []), query)
    match = re.fullmatch(r"pipeline_schedules/(\d+)/pipelines", tail)
    if match and method == "GET":
        chosen = [p for p in world["pipelines"] if p.get("schedule_id") == int(match.group(1))]
        page([public(p) for p in chosen], query)
    if tail == "pipeline" and method == "POST":
        world["next_id"] += 1
        created = pipeline(world["next_id"], 300 + len(world["pipelines"]), body.get("ref", "main"), "created",
                           BASE, "api", stamp(), [job(world["next_id"] * 10, "test", "test", "created")])
        world["pipelines"].insert(0, created)
        return 201, public(created)
    match = re.fullmatch(r"jobs/(\d+)(?:/(.*))?", tail)
    if match:
        job_id, rest = int(match.group(1)), match.group(2) or ""
        found = next((j for p in world["pipelines"] for j in p["jobs"] if j["id"] == job_id), None)
        if not found:
            fail(404, "404 Job Not Found")
        if rest == "":
            return 200, found
        if rest == "trace":
            traces = world.get("traces", {})
            return 200, traces.get(str(job_id), traces.get(job_id, TRACE))
        # The designed Actions world's jobs upload its archives; F6's job 44 uploads this file's.
        import world_actions
        designed = world_actions.GITLAB_ARCHIVES.get(job_id)
        members = world_actions.DESIGN_FILES.get(designed, {}) if designed else ARTIFACT_FILES
        if rest == "artifacts" and method == "GET":
            if not found.get("artifacts_file"):
                fail(404, "404 Not Found")
            archive = world_actions.artifact_zip(designed) if designed else artifact_zip()
            raise Reply(200, archive, {"Content-Type": "application/zip",
                                       "Content-Disposition": 'attachment; filename="artifacts.zip"'})
        if rest.startswith("artifacts/") and method == "GET":
            wanted = urllib.parse.unquote(rest[len("artifacts/"):])
            if not found.get("artifacts_file") or wanted not in members:
                fail(404, "404 File Not Found")
            raise Reply(200, members[wanted], {"Content-Type": "application/octet-stream"})
        if rest == "retry" and method == "POST":
            found["status"] = "pending"
            return 201, found
    match = re.fullmatch(r"repository/commits/([^/]+)/diff", tail)
    if match:
        page(world.get("commit_diffs", {}).get(match.group(1), DIFF_13), query)
    if tail == "repository/files/.gitlab-ci.yml/raw":
        return 200, "stages: [test, build, deploy]\ntest:\n  stage: test\n  script: cargo test\n"
    fail(404, "404 Not Found")


def edit(item, body):
    for key in ("title", "description"):
        if key in body:
            item[key] = body[key]
    if body.get("state_event") == "close":
        item["state"] = "closed"
    if body.get("state_event") == "reopen":
        item["state"] = "opened"
    for label in filter(None, (body.get("add_labels") or "").split(",")):
        if label not in item["labels"]:
            item["labels"].append(label)
    for label in filter(None, (body.get("remove_labels") or "").split(",")):
        if label in item["labels"]:
            item["labels"].remove(label)
    if "assignee_ids" in body:
        item["assignees"] = ids_to_users(body["assignee_ids"])
    if "reviewer_ids" in body:
        item["reviewers"] = ids_to_users(body["reviewer_ids"])
    if "milestone_id" in body:
        item["milestone"] = next((m for m in MILESTONES if m["id"] == body["milestone_id"]), None)
    item["updated_at"] = stamp()


def item_route(world, kind, item, method, rest, query, body, viewer):
    is_mr = kind == "merge_requests"
    if rest == "":
        if method == "GET":
            return 200, mr_detail(world, item) if is_mr else public(item)
        if method == "PUT":
            edit(item, body)
            return 200, mr_detail(world, item) if is_mr else public(item)
    if rest == "notes":
        if method == "POST":
            world["next_id"] += 1
            created = note(world["next_id"], viewer if viewer in USERS else "rowan", body.get("body", ""),
                           stamp())
            item["notes"].append(created)
            item["user_notes_count"] += 1
            return 201, created
        notes = list(item["notes"])
        if query.get("sort") == "desc":
            notes.reverse()
        page(notes, query)
    if rest in ("subscribe", "unsubscribe") and method == "POST":
        wanted = rest == "subscribe"
        if item["subscribed"] == wanted:
            return 304, None
        item["subscribed"] = wanted
        return 201, public(item)
    if rest in ("resource_label_events", "resource_milestone_events", "resource_state_events") and method == "GET":
        page(item.get(rest, []), query)
    if rest == "award_emoji" and method == "GET":
        page(item.get("award_emoji", []), query)
    if not is_mr:
        if rest == "closed_by":
            return 200, [public(find(world["merge_requests"], "iid", n)) for n in item["closed_by"]]
        fail(404, "404 Not Found")
    if rest == "approvals":
        return 200, mr_approvals(item)
    if rest == "reviewers":
        return 200, mr_reviewers(item)
    if rest == "commits":
        page(item["commits"], query)
    if rest == "diffs":
        page(item["diffs"], query)
    if rest == "changes":
        return 200, dict(public(item), changes=item["diffs"])
    if rest == "closes_issues":
        return 200, [public(find(world["issues"], "iid", n)) for n in item["closes"]]
    if rest == "discussions":
        if method == "POST":
            world["next_id"] += 1
            pos = body.get("position")
            created = {"id": f"d{world['next_id']}", "individual_note": pos is None, "notes": [
                note(world["next_id"], viewer if viewer in USERS else "rowan", body.get("body", ""),
                     stamp(), kind="DiffNote" if pos else None, position=pos, resolvable=bool(pos))]}
            item["discussions"].append(created)
            return 201, created
        page(item["discussions"], query)
    match = re.fullmatch(r"discussions/([^/]+)(/notes)?", rest)
    if match:
        discussion = find(item["discussions"], "id", match.group(1))
        if match.group(2) and method == "POST":
            world["next_id"] += 1
            first = discussion["notes"][0]
            created = note(world["next_id"], viewer if viewer in USERS else "rowan", body.get("body", ""),
                           stamp(), kind=first.get("type"), position=first.get("position"),
                           resolvable=first.get("resolvable", False))
            discussion["notes"].append(created)
            return 201, created
        if method == "PUT":
            for each in discussion["notes"]:
                if each.get("resolvable"):
                    each["resolved"] = bool(body.get("resolved"))
            return 200, discussion
    if rest == "draft_notes" and method == "POST":
        if not world["draft_notes"]:
            fail(404, "404 Not Found")
        world["next_id"] += 1
        item["drafts"].append({"id": world["next_id"], "note": body.get("note", ""), "position": body.get("position")})
        return 201, item["drafts"][-1]
    if rest == "draft_notes/bulk_publish" and method == "POST":
        for draft in item["drafts"]:
            world["next_id"] += 1
            pos = draft.get("position")
            created = note(world["next_id"], viewer if viewer in USERS else "rowan", draft["note"],
                           stamp(), kind="DiffNote" if pos else None, position=pos,
                           resolvable=bool(pos))
            if pos:
                item["discussions"].append({"id": f"d{world['next_id']}", "individual_note": False, "notes": [created]})
            else:
                item["notes"].append(created)
        item["drafts"] = []
        return 204, None
    if rest == "approve" and method == "POST":
        if body.get("sha") and body["sha"] != item["sha"]:
            fail(409, "SHA does not match HEAD of source branch")
        if viewer not in item["approved_by"]:
            item["approved_by"].append(viewer)
        item["reviewer_states"][viewer] = "approved"
        return 201, mr_approvals(item)
    if rest == "unapprove" and method == "POST":
        if viewer in item["approved_by"]:
            item["approved_by"].remove(viewer)
        return 201, mr_approvals(item)
    if rest == "rebase" and method == "PUT":
        return 202, {"rebase_in_progress": True}
    if rest == "merge" and method == "PUT":
        if body.get("sha") and body["sha"] != item["sha"]:
            fail(409, "SHA does not match HEAD of source branch")
        if item["state"] != "opened":
            fail(405, "Method Not Allowed")
        if body.get("merge_when_pipeline_succeeds"):
            item["merge_when_pipeline_succeeds"] = True
            return 200, mr_detail(world, item)
        item["state"] = "merged"
        item["updated_at"] = stamp()
        item["squash"] = bool(body.get("squash"))
        return 200, mr_detail(world, item)
    fail(404, "404 Not Found")


if __name__ == "__main__":
    print(json.dumps(seed())[:200])
