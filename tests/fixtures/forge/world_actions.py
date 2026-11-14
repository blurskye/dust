"""GitHub Actions for the fake forge (`server.py --world actions`, FORGE.md §8.2, F6).

One repository, `octo/orbit`, with workflows, runs (one on its second attempt, one fork run
waiting for approval, one in progress), jobs with and without `steps[]`, timestamped logs with
`##[group]` markers, artifacts, notifications and a few pull requests whose check rollups hold
duplicate `(workflow, name)` contexts. Shapes follow GitHub's REST v3 and the GraphQL documents
the GitHub provider sends; the aliased `*Batch` documents (§7.5) are answered alias by alias.
Logs and artifact archives are `302`s to `/_blob/` on another origin (`localhost` for
`127.0.0.1`), like GitHub's blob storage. Writes change the world, so a test can read its own
write back. `state["actions"]` holds the world once seeded; a test may seed it itself.
"""
import copy
import io
import re
import zipfile

REPO = "octo/orbit"
SHA = "a" * 40
FORK_SHA = "f" * 40


class Reply(Exception):
    def __init__(self, status, body, headers=None):
        super().__init__(status)
        self.status, self.body, self.headers = status, body, headers or {}


def fail(status, message):
    raise Reply(status, {"message": message, "documentation_url": "https://docs.github.com/rest"})


# --------------------------------------------------------------------------- blobs

LOGS = {
    # Job 602 lists no steps: the provider cuts it by its `##[group]` sections.
    602: "﻿2026-11-14T05:00:01.1000000Z ##[group]Set up job\n"
         "2026-11-14T05:00:01.2000000Z Runner version 2.330.0\n"
         "2026-11-14T05:00:01.3000000Z ##[endgroup]\n"
         "2026-11-14T05:00:02.0000000Z ##[group]Run cargo test\n"
         "2026-11-14T05:00:02.1000000Z cargo test --locked\n"
         "2026-11-14T05:00:02.2000000Z ##[endgroup]\n"
         "2026-11-14T05:00:03.0000000Z running 3 tests\n"
         "2026-11-14T05:00:04.0000000Z test retries_uploads ... \x1b[31mFAILED\x1b[0m\n"
         "2026-11-14T05:00:05.0000000Z ##[error]Process completed with exit code 101.\n",
    # Job 601 lists its steps: lines go to the step running at their time.
    601: "﻿2026-11-14T05:00:00.5000000Z Current runner version: '2.330.0'\n"
         "2026-11-14T05:00:01.6000000Z ##[group]Run cargo build --release\n"
         "2026-11-14T05:00:01.7000000Z cargo build --release\n"
         "2026-11-14T05:00:01.8000000Z ##[endgroup]\n"
         "2026-11-14T05:00:08.0000000Z    Finished release [optimized] target(s)\n"
         "2026-11-14T05:00:10.0000000Z Cleaning up orphan processes\n",
}

ARTIFACT_FILES = {
    701: {"coverage/index.html": b"<html>coverage 81%</html>\n", "lcov.info": b"TN:\nSF:src/lib.rs\nend_of_record\n"},
    703: {"orbit": b"\x7fELF fake binary\n"},
}


def artifact_zip(artifact_id):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        files = ARTIFACT_FILES.get(artifact_id, {"report.txt": f"artifact {artifact_id}\n".encode()})
        for path, contents in files.items():
            info = zipfile.ZipInfo(path, date_time=(2026, 11, 14, 5, 1, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, contents)
    return buffer.getvalue()


def blob(path):
    """`/_blob/<name>` bodies: `log-<job>.txt` and `artifact-<id>.zip` (an artifact this world
    does not hold gets a one-file archive; the Gitea world redirects here too); None otherwise."""
    match = re.fullmatch(r"/_blob/log-(\d+)\.txt", path)
    if match:
        return "text/plain; charset=utf-8", LOGS.get(int(match.group(1)), "").encode()
    match = re.fullmatch(r"/_blob/artifact-(\d+)\.zip", path)
    if match:
        return "application/zip", artifact_zip(int(match.group(1)))
    return None


def elsewhere(request, name):
    """A redirect target on another origin than the API (the token must not follow)."""
    host = request.headers.get("host", "127.0.0.1")
    host = host.replace("127.0.0.1", "localhost", 1)
    return f"http://{host}/_blob/{name}"


# --------------------------------------------------------------------------- the world


def step(number, name, conclusion, started, completed):
    return {"number": number, "name": name, "status": "completed", "conclusion": conclusion,
            "started_at": started, "completed_at": completed}


def job(job_id, run_id, attempt, name, status, conclusion, started, completed, steps):
    return {"id": job_id, "run_id": run_id, "run_attempt": attempt, "name": name, "status": status,
            "conclusion": conclusion, "started_at": started, "completed_at": completed,
            "head_sha": SHA, "steps": steps}


def run(run_id, number, workflow, title, branch, sha, event, status, conclusion, created, updated,
        attempt=1, owner="octo", pulls=()):
    return {"id": run_id, "run_number": number, "name": workflow["name"], "display_title": title,
            "workflow_id": workflow["id"], "path": workflow["path"], "status": status,
            "conclusion": conclusion, "head_branch": branch, "head_sha": sha, "event": event,
            "created_at": created, "updated_at": updated, "run_attempt": attempt,
            "pull_requests": [{"number": number} for number in pulls],
            "head_repository": {"full_name": f"{owner}/orbit", "owner": {"login": owner}}}


WORKFLOWS = [
    {"id": 31, "name": "CI", "path": ".github/workflows/ci.yml", "state": "active", "dispatch": True},
    {"id": 32, "name": "Release", "path": ".github/workflows/release.yml", "state": "active", "dispatch": False},
]


def seed():
    ci, release = WORKFLOWS
    build_steps = [step(1, "Set up job", "success", "2026-11-14T05:00:00Z", "2026-11-14T05:00:01Z"),
                   step(2, "Run cargo build --release", "success", "2026-11-14T05:00:01Z", "2026-11-14T05:00:09Z"),
                   step(3, "Complete job", "success", "2026-11-14T05:00:09Z", "2026-11-14T05:00:10Z")]
    runs = [
        run(503, 12, release, "Release v1.4", "main", SHA, "push", "in_progress", None,
            "2026-11-14T05:20:00Z", "2026-11-14T05:21:00Z"),
        run(502, 11, ci, "Fix typo", "patch-1", FORK_SHA, "pull_request", "completed", "action_required",
            "2026-11-14T05:15:00Z", "2026-11-14T05:15:00Z", owner="forker", pulls=(12,)),
        run(501, 10, ci, "Retry flaky uploads", "retry-uploads", SHA, "pull_request", "completed", "failure",
            "2026-11-14T05:00:00Z", "2026-11-14T05:10:00Z", attempt=2, pulls=(9,)),
    ]
    jobs = {
        503: [job(621, 503, 1, "publish", "in_progress", None, "2026-11-14T05:20:05Z", None, [])],
        502: [],
        501: [job(601, 501, 2, "build", "completed", "success", "2026-11-14T05:00:00Z", "2026-11-14T05:00:10Z",
                  build_steps),
              job(602, 501, 2, "test", "completed", "failure", "2026-11-14T05:00:01Z", "2026-11-14T05:00:05Z", []),
              job(603, 501, 2, "deploy", "completed", "skipped", None, None, [])],
    }
    attempts = {"501/1": {"conclusion": "failure", "updated_at": "2026-11-14T04:50:00Z",
                          "jobs": [job(611, 501, 1, "build", "completed", "success", "2026-11-14T04:40:00Z",
                                       "2026-11-14T04:41:00Z", build_steps),
                                   job(612, 501, 1, "test", "completed", "failure", "2026-11-14T04:41:00Z",
                                       "2026-11-14T04:45:00Z", [])]}}
    artifacts = {501: [{"id": 701, "name": "coverage", "expired": False, "expires_at": "2026-12-14T05:10:00Z"},
                       {"id": 702, "name": "old-logs", "expired": True, "expires_at": "2026-11-01T00:00:00Z"},
                       {"id": 703, "name": "orbit-linux", "expired": False, "expires_at": "2026-12-14T05:10:00Z"}]}
    notifications = [
        {"id": "9001", "unread": True, "reason": "review_requested", "updated_at": "2026-11-14T05:30:00Z",
         "subject": {"title": "Retry flaky uploads", "type": "PullRequest",
                     "url": f"https://api.github.com/repos/{REPO}/pulls/9", "latest_comment_url": None}},
        {"id": "9002", "unread": True, "reason": "ci_activity", "updated_at": "2026-11-14T05:10:00Z",
         "subject": {"title": "CI workflow run failed for retry-uploads branch", "type": "CheckSuite",
                     "url": None, "latest_comment_url": None}},
        {"id": "9003", "unread": False, "reason": "mention", "updated_at": "2026-11-13T12:00:00Z",
         "subject": {"title": "Pipelines tab shows stale status", "type": "Issue",
                     "url": f"https://api.github.com/repos/{REPO}/issues/4", "latest_comment_url": None}},
    ]
    checks = [
        # The same check twice (a rerun): the newer start wins, so the failure is superseded.
        {"name": "test", "workflow": "CI", "status": "COMPLETED", "conclusion": "FAILURE",
         "startedAt": "2026-11-14T04:41:00Z", "run": 501},
        {"name": "test", "workflow": "CI", "status": "COMPLETED", "conclusion": "SUCCESS",
         "startedAt": "2026-11-14T05:00:01Z", "run": 501},
        # One name in two workflows: both stay, prefixed with their workflow.
        {"name": "lint", "workflow": "CI", "status": "COMPLETED", "conclusion": "SUCCESS",
         "startedAt": "2026-11-14T05:00:00Z", "run": 501},
        {"name": "lint", "workflow": "Docs", "status": "COMPLETED", "conclusion": "SUCCESS",
         "startedAt": "2026-11-14T05:00:00Z", "run": 504},
    ]
    pulls = {str(number): {"number": number, "title": title, "head": head, "state": "OPEN",
                           "updatedAt": f"2026-11-14T05:{number:02d}:00Z", "checks": checks if number == 9 else []}
             for number, title, head in ((9, "Retry flaky uploads", SHA), (10, "Cache the registry", "b" * 40),
                                         (11, "Split settings", "c" * 40), (12, "Fix typo", FORK_SHA))}
    return {"runs": runs, "jobs": {str(key): value for key, value in jobs.items()}, "attempts": attempts,
            "artifacts": {str(key): value for key, value in artifacts.items()},
            "notifications": notifications, "pulls": pulls, "next_run": 600}


def html(request, tail):
    return f"https://{request.host}/{REPO}/{tail}"


def run_body(request, item):
    body = copy.deepcopy(item)
    body["html_url"] = html(request, f"actions/runs/{item['id']}")
    return body


def job_body(request, item):
    body = copy.deepcopy(item)
    body["html_url"] = html(request, f"actions/runs/{item['run_id']}/job/{item['id']}")
    return body


def artifact_body(request, run_id, item):
    size = len(artifact_zip(item["id"]))
    return {"id": item["id"], "node_id": f"MDg6QXJ0aWZhY3Q{item['id']}", "name": item["name"],
            "size_in_bytes": size, "expired": item["expired"], "expires_at": item["expires_at"],
            "url": f"https://api.github.com/repos/{REPO}/actions/artifacts/{item['id']}",
            "archive_download_url": f"https://api.github.com/repos/{REPO}/actions/artifacts/{item['id']}/zip",
            "workflow_run": {"id": run_id, "head_sha": SHA}}


def find_run(world, run_id):
    found = next((item for item in world["runs"] if item["id"] == run_id), None)
    if found is None:
        fail(404, "Not Found")
    return found


def find_job(world, job_id):
    for jobs in world["jobs"].values():
        for item in jobs:
            if item["id"] == job_id:
                return item
    for attempt in world["attempts"].values():
        for item in attempt["jobs"]:
            if item["id"] == job_id:
                return item
    fail(404, "Not Found")


def paged(items, query):
    per_page = max(1, min(100, int(query.get("per_page") or 30)))
    number = max(1, int(query.get("page") or 1))
    return items[(number - 1) * per_page: number * per_page]


def answer(state, request):
    world = state.get("actions")
    if world is None:
        world = state["actions"] = seed()
    if request.graphql:
        return 200, graphql(world, request)
    if request.rest == "user":
        return 200, {"login": state.get("viewer", "tester"), "id": 1}
    if request.rest.startswith("notifications"):
        return notifications(world, request)
    if request.tail is None or request.repo != REPO:
        fail(404, "Not Found")
    if request.method == "GET":
        return read(world, request, request.tail, request.query)
    return write(world, request, request.tail, request.json or {})


def notifications(world, request):
    method, rest, query = request.method, request.rest, request.query
    if rest == "notifications" and method == "GET":
        items = [item for item in world["notifications"]
                 if (query.get("all") == "true" or item["unread"])
                 and (not query.get("since") or item["updated_at"] > query["since"])]
        items.sort(key=lambda item: item["updated_at"], reverse=True)
        return 200, [dict(item, repository={"full_name": REPO}) for item in paged(items, query)]
    if rest == "notifications" and method == "PUT":
        for item in world["notifications"]:
            item["unread"] = False
        return 205, None
    match = re.fullmatch(r"notifications/threads/(\d+)", rest)
    if match and method == "PATCH":
        for item in world["notifications"]:
            if item["id"] == match.group(1):
                item["unread"] = False
        return 205, None
    fail(404, "Not Found")


def read(world, request, tail, query):
    if tail == "":
        return 200, {"full_name": REPO, "default_branch": "main", "node_id": "REPO_NODE",
                     "html_url": f"https://{request.host}/{REPO}", "fork": False,
                     "permissions": {"admin": False, "push": True, "pull": True}}
    if tail == "actions/workflows":
        flows = [{key: value for key, value in flow.items() if key != "dispatch"} for flow in WORKFLOWS]
        return 200, {"total_count": len(flows), "workflows": flows}
    match = re.fullmatch(r"actions/(?:workflows/(\d+)/)?runs", tail)
    if match:
        chosen = [item for item in world["runs"]
                  if (not match.group(1) or item["workflow_id"] == int(match.group(1)))
                  and (not query.get("branch") or item["head_branch"] == query["branch"])
                  and (not query.get("event") or item["event"] == query["event"])
                  and (not query.get("status") or query["status"] in (item["status"], item["conclusion"]))]
        return 200, {"total_count": len(chosen), "workflow_runs": [run_body(request, item)
                                                                  for item in paged(chosen, query)]}
    match = re.fullmatch(r"actions/runs/(\d+)(?:/attempts/(\d+))?(/jobs|/artifacts)?", tail)
    if match:
        found = find_run(world, int(match.group(1)))
        attempt, part = match.group(2), match.group(3)
        if part == "/artifacts":
            items = [artifact_body(request, found["id"], item)
                     for item in world["artifacts"].get(str(found["id"]), [])]
            return 200, {"total_count": len(items), "artifacts": paged(items, query)}
        jobs = world["jobs"].get(str(found["id"]), [])
        body = run_body(request, found)
        if attempt and int(attempt) != found["run_attempt"]:
            earlier = world["attempts"].get(f"{found['id']}/{attempt}")
            if earlier is None:
                fail(404, "Not Found")
            body.update(run_attempt=int(attempt), conclusion=earlier["conclusion"],
                        updated_at=earlier["updated_at"])
            jobs = earlier["jobs"]
        if part == "/jobs":
            if query.get("filter") not in (None, "latest", "all"):
                fail(422, "Invalid filter")
            items = [job_body(request, item) for item in jobs]
            return 200, {"total_count": len(items), "jobs": paged(items, query)}
        return 200, body
    match = re.fullmatch(r"actions/jobs/(\d+)/logs", tail)
    if match:
        found = find_job(world, int(match.group(1)))
        if found["started_at"] is None:
            fail(404, "Not Found")
        raise Reply(302, b"", {"Location": elsewhere(request, f"log-{found['id']}.txt")})
    match = re.fullmatch(r"actions/jobs/(\d+)", tail)
    if match:
        return 200, job_body(request, find_job(world, int(match.group(1))))
    match = re.fullmatch(r"actions/artifacts/(\d+)/zip", tail)
    if match:
        artifact_id = int(match.group(1))
        item = next((item for items in world["artifacts"].values() for item in items
                     if item["id"] == artifact_id), None)
        if item is None:
            fail(404, "Not Found")
        if item["expired"]:
            fail(410, "Artifact has expired")
        raise Reply(302, b"", {"Location": elsewhere(request, f"artifact-{artifact_id}.zip")})
    match = re.fullmatch(r"pulls/(\d+)", tail)
    if match:
        pull = world["pulls"].get(match.group(1))
        if pull is None:
            fail(404, "Not Found")
        return 200, {"number": pull["number"], "title": pull["title"], "state": "open",
                     "head": {"sha": pull["head"]}, "updated_at": pull["updatedAt"]}
    fail(404, "Unexpected fixture read: " + tail)


def write(world, request, tail, body):
    method = request.method
    match = re.fullmatch(r"actions/workflows/(\d+)/dispatches", tail)
    if match and method == "POST":
        flow = next((flow for flow in WORKFLOWS if flow["id"] == int(match.group(1))), None)
        if flow is None:
            fail(404, "Not Found")
        if not flow["dispatch"]:
            fail(422, "Workflow does not have 'workflow_dispatch' trigger")
        if not body.get("ref"):
            fail(422, "Invalid request.\n\n\"ref\" wasn't supplied.")
        world["next_run"] += 1
        world["runs"].insert(0, run(world["next_run"], world["next_run"], flow, flow["name"], body["ref"], SHA,
                                    "workflow_dispatch", "queued", None, "2026-11-14T06:00:00Z",
                                    "2026-11-14T06:00:00Z"))
        world["jobs"][str(world["next_run"])] = []
        world.setdefault("dispatched", []).append({"workflow": flow["id"], "ref": body["ref"],
                                                   "inputs": body.get("inputs") or {}})
        return 204, None
    match = re.fullmatch(r"actions/runs/(\d+)/(rerun|rerun-failed-jobs|cancel|approve)", tail)
    if match and method == "POST":
        found = find_run(world, int(match.group(1)))
        action = match.group(2)
        if action == "cancel":
            if found["status"] == "completed":
                fail(409, "Cannot cancel a workflow run that is completed.")
            found.update(status="completed", conclusion="cancelled")
            return 202, {}
        if action == "approve":
            if found["conclusion"] != "action_required":
                fail(403, "This run is not from a fork pull request")
            found.update(status="queued", conclusion=None)
            return 201, {}
        if found["status"] != "completed":
            fail(403, "This workflow is already running")
        found.update(status="queued", conclusion=None, run_attempt=found["run_attempt"] + 1)
        world.setdefault("reruns", []).append({"run": found["id"], "scope": action,
                                               "debug": bool(body.get("enable_debug_logging"))})
        return 201, {}
    match = re.fullmatch(r"actions/jobs/(\d+)/rerun", tail)
    if match and method == "POST":
        found = find_job(world, int(match.group(1)))
        found.update(status="queued", conclusion=None)
        return 201, {}
    fail(404, "Unexpected fixture write: " + method + " " + tail)


# --------------------------------------------------------------------------- GraphQL


def gql_check(item):
    return {"__typename": "CheckRun", "name": item["name"], "status": item["status"],
            "conclusion": item["conclusion"], "startedAt": item["startedAt"], "completedAt": item["startedAt"],
            "detailsUrl": f"https://github.com/{REPO}/actions/runs/{item['run']}",
            "checkSuite": {"workflowRun": {"databaseId": item["run"], "workflow": {"name": item["workflow"]}}}}


def rollup(checks):
    words = [item["conclusion"] or item["status"] for item in checks]
    if not words:
        return None
    if "FAILURE" in words:
        return "FAILURE"
    if any(word in ("IN_PROGRESS", "QUEUED", "PENDING") for word in words):
        return "PENDING"
    return "SUCCESS"


def pull_node(pull, fingerprint):
    state = rollup(pull["checks"])
    status = None if state is None else {"state": state}
    if fingerprint:
        return {"state": pull["state"], "mergeable": "MERGEABLE", "headRefOid": pull["head"],
                "updatedAt": pull["updatedAt"], "commits": {"nodes": [{"commit": {"statusCheckRollup": status}}]}}
    number = pull["number"]
    return {"id": f"PR_NODE_{number}", "number": number, "title": pull["title"], "body": "", "state": pull["state"],
            "isDraft": False, "url": f"https://github.com/{REPO}/pull/{number}", "createdAt": pull["updatedAt"],
            "updatedAt": pull["updatedAt"], "author": {"login": "maya"}, "baseRefName": "main",
            "headRefName": f"topic-{number}", "headRefOid": pull["head"], "baseRefOid": "e" * 40,
            "headRepository": {"nameWithOwner": REPO}, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "reviewDecision": None, "additions": 1, "deletions": 0, "changedFiles": 1,
            "comments": {"totalCount": 0}, "assignees": {"nodes": []}, "labels": {"nodes": []}, "milestone": None,
            "projectItems": {"nodes": []}, "viewerSubscription": "SUBSCRIBED", "reviewRequests": {"nodes": []},
            "reviews": {"nodes": []}, "commits": {"nodes": []},
            "files": {"pageInfo": {"hasNextPage": False, "endCursor": None},
                      "nodes": [{"path": "src/lib.rs", "additions": 1, "deletions": 0, "changeType": "MODIFIED"}]},
            "closingIssuesReferences": {"nodes": []},
            "latest": {"nodes": [{"commit": {"statusCheckRollup": None if status is None else dict(
                status, contexts={"nodes": [gql_check(item) for item in pull["checks"]]})}}]}}


def graphql(world, request):
    operation = request.operation or ""
    base = operation[:-len("Batch")] if operation.endswith("Batch") else operation
    if base not in ("PullRequestDetail", "PullRequestFingerprint"):
        return {"errors": [{"message": "Unexpected fixture operation " + operation}]}
    fingerprint = base == "PullRequestFingerprint"
    if operation.endswith("Batch"):
        repository, errors = {}, []
        for alias, number in re.findall(r"(p\d+): pullRequest\(number: (\d+)\)", request.document):
            pull = world["pulls"].get(number)
            repository[alias] = pull_node(pull, fingerprint) if pull else None
            if pull is None:
                errors.append({"type": "NOT_FOUND", "path": ["repository", alias],
                               "message": f"Could not resolve to a PullRequest with the number of {number}."})
        body = {"data": {"repository": repository}}
        if errors:
            body["errors"] = errors
        return body
    number = str(request.variables.get("number"))
    pull = world["pulls"].get(number)
    if pull is None:
        return {"data": {"repository": {"pullRequest": None}},
                "errors": [{"type": "NOT_FOUND", "path": ["repository", "pullRequest"],
                            "message": f"Could not resolve to a PullRequest with the number of {number}."}]}
    return {"data": {"repository": {"pullRequest": pull_node(pull, fingerprint)}}}
