"""GitHub Actions for the fake forge (`server.py --world actions`).

One repository, `octo/orbit`, with workflows, runs (one on its second attempt, one fork run
waiting for approval, one in progress), jobs with and without `steps[]`, timestamped logs with
`##[group]` markers, artifacts, notifications and a few pull requests whose check rollups hold
duplicate `(workflow, name)` contexts. Shapes follow GitHub's REST v3 and the GraphQL documents
the GitHub provider sends; the aliased `*Batch` documents are answered alias by alias.
Logs and artifact archives are `302`s to `/_blob/` on another origin (`localhost` for
`127.0.0.1`), like GitHub's blob storage. Writes change the world, so a test can read its own
write back. `state["actions"]` holds the world once seeded; a test may seed it itself.
"""
import copy
import hashlib
import io
import re
import time
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
        files = ARTIFACT_FILES.get(artifact_id) or DESIGN_FILES.get(artifact_id) or {
            "report.txt": f"artifact {artifact_id}\n".encode()}
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
        job_id = int(match.group(1))
        return "text/plain; charset=utf-8", (LOGS.get(job_id) or DESIGN_LOGS.get(job_id, "")).encode()
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
    # Turned off on the forge: listed muted, `(off)`, and `d` refuses it.
    {"id": 33, "name": "Nightly", "path": ".github/workflows/nightly.yml", "state": "disabled_manually",
     "dispatch": True},
]


def seed():
    ci, release = WORKFLOWS[:2]
    build_steps = [step(1, "Set up job", "success", "2026-11-14T05:00:00Z", "2026-11-14T05:00:01Z"),
                   step(2, "Run cargo build --release", "success", "2026-11-14T05:00:01Z", "2026-11-14T05:00:09Z"),
                   step(3, "Complete job", "success", "2026-11-14T05:00:09Z", "2026-11-14T05:00:10Z")]
    runs = [
        # Running work starts a few minutes before the server clock, so it reads `running 3m`
        # rather than however long ago this fixture was written.
        run(503, 12, release, "Release v1.4", "main", SHA, "push", "in_progress", None,
            iso(time.time() - 4 * 60), iso(time.time() - 3 * 60)),
        run(502, 11, ci, "Fix typo", "patch-1", FORK_SHA, "pull_request", "completed", "action_required",
            "2026-11-14T05:15:00Z", "2026-11-14T05:15:00Z", owner="forker", pulls=(12,)),
        run(501, 10, ci, "Retry flaky uploads", "retry-uploads", SHA, "pull_request", "completed", "failure",
            "2026-11-14T05:00:00Z", "2026-11-14T05:10:00Z", attempt=2, pulls=(9,)),
    ]
    jobs = {
        503: [job(621, 503, 1, "publish", "in_progress", None, iso(time.time() - 4 * 60 + 5), None, [])],
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
    world = design_world(state)
    if request.graphql:
        return 200, graphql(world, request)
    if request.rest == "user":
        return 200, {"login": state.get("viewer", "rowan"), "id": 1}
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


def settle(world):
    """Runs whose cancel was asked for stop once its time has come, with their jobs."""
    for item in world["runs"]:
        if item.get("cancel_at") and time.time() >= item["cancel_at"]:
            del item["cancel_at"]
            item.update(status="completed", conclusion="cancelled")
            for listed in world["jobs"].get(str(item["id"]), []):
                if listed["status"] != "completed":
                    listed.update(status="completed", conclusion="cancelled")


def read(world, request, tail, query):
    settle(world)
    if tail == "":
        return 200, {"full_name": REPO, "default_branch": "main", "node_id": "REPO_NODE",
                     "html_url": f"https://{request.host}/{REPO}", "fork": False,
                     "permissions": {"admin": False, "push": True, "pull": True}}
    if tail == "actions/workflows":
        flows = [{key: value for key, value in flow.items() if key != "dispatch"}
                 for flow in world.get("workflows", WORKFLOWS)]
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
        flow = next((flow for flow in world.get("workflows", WORKFLOWS) if flow["id"] == int(match.group(1))), None)
        if flow is None:
            fail(404, "Not Found")
        if not flow["dispatch"]:
            fail(422, "Workflow does not have 'workflow_dispatch' trigger")
        if not body.get("ref"):
            fail(422, "Invalid request.\n\n\"ref\" wasn't supplied.")
        world["next_run"] += 1
        world["runs"].insert(0, run(world["next_run"], world["next_run"], flow, flow["name"], body["ref"], SHA,
                                    "workflow_dispatch", "queued", None, iso(time.time()),
                                    iso(time.time())))
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
            if world.get("slow_cancel"):
                # As GitHub does, the run stops a little later (`Cancellation requested`).
                found["cancel_at"] = time.time() + 8
                return 202, {}
            found.update(status="completed", conclusion="cancelled")
            return 202, {}
        if action == "approve":
            if found["conclusion"] != "action_required":
                fail(403, "This run is not from a fork pull request")
            found.update(status="queued", conclusion=None, run_started_at=iso(time.time()), updated_at=iso(time.time()))
            return 201, {}
        if found["status"] != "completed":
            fail(403, "This workflow is already running")
        # As on GitHub, a re-run keeps `created_at` but starts again now.
        found.update(status="queued", conclusion=None, run_attempt=found["run_attempt"] + 1,
                     run_started_at=iso(time.time()), updated_at=iso(time.time()))
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
    if base == "PullRequestList":
        # Every section lists the same pull requests, newest first (R9-05: a check's run and back).
        pulls = sorted(world["pulls"].values(), key=lambda pull: pull["updatedAt"], reverse=True)
        nodes = []
        for pull in pulls:
            node = dict(pull_node(pull, False), __typename="PullRequest", repository={"nameWithOwner": REPO})
            state = rollup(pull["checks"])
            node["commits"] = {"nodes": [{"commit": {"statusCheckRollup": None if state is None else {
                "state": state, "contexts": {"checkRunCountsByState": [], "statusContextCountsByState": []}}}}]}
            nodes.append(node)
        return {"data": {"search": {"issueCount": len(nodes), "nodes": nodes,
                                    "pageInfo": {"hasNextPage": False, "endCursor": None}}}}
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


# --------------------------------------------------------------------------- the designed world
#
# `state["actions_seed"] = "design"` swaps the runs for the designed world: two
# workflows (Quality checks, Preview deployment), ten runs over four days relative to now (one
# failing, one running, three by maya), jobs with steps and logs whose lines fall inside their
# steps' times, and two artifacts. The same world renders for GitHub (here), GitLab
# (`gitlab_world`) and Gitea / Forgejo (`gitea_answer`), so every provider is shot against it.

DESIGN_FLOWS = [
    {"id": 41, "name": "Quality checks", "path": ".github/workflows/quality.yml", "state": "active", "dispatch": True},
    {"id": 42, "name": "Preview deployment", "path": ".github/workflows/preview.yml", "state": "active",
     "dispatch": True},
    # The Workflows panel: one that has never run, one turned off.
    {"id": 43, "name": "Publish package", "path": ".github/workflows/publish.yml", "state": "active",
     "dispatch": True},
    {"id": 44, "name": "Nightly build", "path": ".github/workflows/nightly.yml", "state": "disabled_manually",
     "dispatch": True},
]
# GitLab's schedules are rows of the Sources panel: one active, one turned off.
GITLAB_SCHEDULES = [
    {"id": 71, "description": "Weekly dependency audit", "ref": "main", "cron": "0 6 * * 1",
     "cron_timezone": "UTC", "active": True, "owner": {"username": "maya"}},
    {"id": 72, "description": "Nightly build", "ref": "main", "cron": "0 3 * * *", "cron_timezone": "UTC",
     "active": False, "owner": {"username": "rowan"}},
]
DESIGN_LOGS = {}
DESIGN_FILES = {}
# The GitLab job that uploaded each designed archive (its `artifacts_file` reports that size).
GITLAB_ARCHIVES = {80701: 8701, 80702: 8703}


def junit_xml():
    """A test report as a runner writes it: one line per case, not padding, so the archive's
    size is what a real one compresses to (A2-08)."""
    cases = [f"  <testcase classname='tests.test_{module}' name='test_{name}_{index}' time='0.{index:03d}'/>"
             for index, (module, name) in enumerate((module, name) for module in ("retry", "upload", "backoff", "store")
                                                    for name in ("limit", "jitter", "timeout", "resume", "checksum",
                                                                 "partial", "expired", "rename"))]
    return ("<?xml version='1.0' encoding='utf-8'?>\n<testsuite name='pytest' tests='32' failures='1'>\n"
            + "\n".join(cases) + "\n</testsuite>\n").encode()


def coverage_xml():
    lines = []
    for index in range(70):
        hits = (index * 37) % 11
        lines.append(f"    <class name='orbit/mod_{index:02d}.py' filename='orbit/mod_{index:02d}.py' "
                     f"line-rate='0.{(index * 53) % 97:02d}'><lines><line number='{index * 7 + 3}' hits='{hits}'/>"
                     f"<line number='{index * 7 + 5}' hits='{hits * 3 % 5}'/></lines></class>")
    return ("<?xml version='1.0'?>\n<coverage line-rate='0.81'>\n  <classes>\n" + "\n".join(lines)
            + "\n  </classes>\n</coverage>\n").encode()


def coverage_json():
    """coverage.py's JSON report: per-file summaries and the lines each file misses."""
    files = []
    for index in range(40):
        statements = 20 + (index * 29) % 140
        missing = [3 + (index * 13 + step * 17) % statements for step in range(index % 9)]
        files.append(f'"orbit/mod_{index:02d}.py": {{"summary": {{"num_statements": {statements}, '
                     f'"missing_lines": {len(missing)}, "percent_covered": '
                     f'{100 * (statements - len(missing)) / statements:.2f}}}, "missing_lines": {sorted(missing)}}}')
    return ('{"meta": {"version": "7.6.4", "format": 3}, "files": {' + ", ".join(files)
            + '}, "totals": {"percent_covered": 81.2}}\n').encode()


def iso(at):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(at))


def log_stamp(at):
    whole = int(at)
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(whole)) + ".%07dZ" % int((at - whole) * 1e7)


SETUP = ["Current runner version: '2.330.0'", "Runner image: ubuntu-24.04 · 20261109.1",
         "Prepare workflow directory", "Prepare all required actions"]
CHECKOUT = ["##[group]Run actions/checkout@v4", "with:", "  repository: octo/orbit", "##[endgroup]",
            "Syncing repository: octo/orbit", "HEAD is now at {sha} {title}"]
CLEANUP = ["Post job cleanup.", "Cleaning up orphan processes"]
UNITTEST = ["##[group]Run python3 -m unittest discover -s tests -v",
            "python3 -m unittest discover -s tests -v", "shell: /usr/bin/bash -e {{0}}", "##[endgroup]",
            "test_backoff_caps_at_limit (test_backoff.BackoffTest.test_backoff_caps_at_limit) ... ok",
            "test_backoff_doubles (test_backoff.BackoffTest.test_backoff_doubles) ... ok",
            "test_queue_summary (test_queue.QueueTest.test_queue_summary) ... ok"]
PASSED = UNITTEST + ["test_retry_limit (test_regression.RegressionTest.test_retry_limit) ... ok", "",
                     "----------------------------------------------------------------------",
                     "Ran 4 tests in 0.003s", "", "OK"]
FAILED = UNITTEST + [
    "test_retry_limit (test_regression.RegressionTest.test_retry_limit) ... FAIL", "",
    "======================================================================",
    "FAIL: test_retry_limit (test_regression.RegressionTest.test_retry_limit)",
    "----------------------------------------------------------------------",
    "Traceback (most recent call last):",
    '  File "/home/runner/work/orbit/orbit/tests/test_regression.py", line 6, in test_retry_limit',
    "    self.assertEqual(retry_limit(), 60)",
    "AssertionError: 120 != 60", "",
    "----------------------------------------------------------------------",
    "Ran 4 tests in 0.004s", "", "FAILED (failures=1)",
    "##[error]Process completed with exit code 1."]
LINT = ["##[group]Run ruff check .", "ruff check .", "##[endgroup]", "All checks passed!"]
BUILD = ["##[group]Run mkdocs build --strict", "mkdocs build --strict", "##[endgroup]",
         "INFO    -  Cleaning site directory",
         "INFO    -  Building documentation to directory: /home/runner/work/orbit/orbit/site",
         "INFO    -  Documentation built in 1.84 seconds"]
UPLOAD = ["##[group]Run actions/upload-artifact@v4", "with:", "  name: site", "##[endgroup]",
          "Uploaded site.zip · 412 KB"]
PUBLISH = ["##[group]Run ./scripts/publish-preview", "./scripts/publish-preview", "##[endgroup]",
           "Publishing to https://preview.orbit.example/pr-{pr}", "Preview is live"]
CANCELLED = ["##[error]The operation was canceled."]
# The preview being published as the shots run: its lines so far.
PUBLISHING = ["##[group]Run ./scripts/publish-preview", "./scripts/publish-preview", "##[endgroup]",
              "Publishing to https://preview.orbit.example/pr-{pr}", "Uploading 38 files (412 KB)",
              "  index.html", "  quickstart/index.html", "  quickstart/install/index.html",
              "  quickstart/first-run/index.html", "  reference/index.html", "  reference/retries/index.html",
              "  assets/site.css", "  assets/search.js", "Uploaded 38 of 38 files",
              "Waiting for the preview host to take the upload…"]


def design_job(job_id, run, name, stage, t0, steps):
    """A job from `(name, conclusion, start, end, lines)` steps (offsets in seconds from `t0`;
    `in_progress` / `queued` / `skipped` steps have no end or no times). Its log puts each line
    strictly inside its step's time."""
    built, log, state = [], [], "success"
    for number, (step_name, conclusion, start, end, lines) in enumerate(steps, 1):
        running = conclusion in ("in_progress", "queued")
        built.append({"number": number, "name": step_name,
                      "status": conclusion if running else "completed",
                      "conclusion": None if running else conclusion,
                      "started_at": None if start is None else iso(t0 + start),
                      "completed_at": None if end is None or running else iso(t0 + end)})
        if conclusion == "failure":
            state = "failure"
        elif conclusion == "cancelled" and state != "failure":
            state = "cancelled"
        elif running:
            state = "in_progress"
        if start is None or conclusion == "queued" or end is None and conclusion != "in_progress":
            continue
        # A running step's lines so far fall in its first minute (the run began two ago).
        span = max((end if end is not None else start + 60) - start, 1)
        lines = [line.format(sha=run["head_sha"][:7], title=run["display_title"], pr=run.get("pr", ""))
                 for line in lines]
        for index, line in enumerate(lines):
            log.append(log_stamp(t0 + start + 0.1 + index * (span - 0.2) / max(len(lines), 1)) + " " + line)
    timed = [step for step in built if step["started_at"]]
    finished = state != "in_progress"
    DESIGN_LOGS[job_id] = "﻿" + "\n".join(log) + "\n" if log else ""
    return {"id": job_id, "run_id": run["id"], "run_attempt": 1, "name": name, "stage": stage,
            "status": "completed" if finished else "in_progress", "conclusion": state if finished else None,
            "started_at": timed[0]["started_at"] if timed else None,
            "completed_at": timed[-1]["completed_at"] if timed and finished else None,
            "head_sha": run["head_sha"], "steps": built}


def quality_jobs(run, t0, outcome):
    first = 100 * run["id"]
    if outcome == "failure":
        tests = [("Set up job", "success", 0, 1, SETUP), ("Checkout", "success", 1, 3, CHECKOUT),
                 ("Run unit tests", "failure", 3, 6, FAILED), ("Compile Python", "skipped", None, None, []),
                 ("Complete job", "success", 6, 7, CLEANUP)]
    elif outcome == "cancelled":
        tests = [("Set up job", "success", 0, 1, SETUP), ("Checkout", "success", 1, 2, CHECKOUT),
                 ("Run unit tests", "cancelled", 2, 4, UNITTEST[:4] + CANCELLED),
                 ("Compile Python", "skipped", None, None, []), ("Complete job", "skipped", None, None, [])]
    else:
        tests = [("Set up job", "success", 0, 1, SETUP), ("Checkout", "success", 1, 3, CHECKOUT),
                 ("Run unit tests", "success", 3, 6, PASSED),
                 ("Compile Python", "success", 6, 7, ["##[group]Run python3 -m compileall -q src", "##[endgroup]"]),
                 ("Complete job", "success", 7, 8, CLEANUP)]
    lint = [("Set up job", "success", 0, 1, SETUP), ("Checkout", "success", 1, 2, CHECKOUT),
            ("Run ruff check", "cancelled" if outcome == "cancelled" else "success", 2, 3,
             LINT[:3] + CANCELLED if outcome == "cancelled" else LINT),
            ("Complete job", "skipped" if outcome == "cancelled" else "success",
             None if outcome == "cancelled" else 3, None if outcome == "cancelled" else 4, CLEANUP)]
    return [design_job(first + 1, run, "Python tests", "test", t0, tests),
            design_job(first + 2, run, "Lint", "lint", t0, lint)]


def preview_jobs(run, t0, outcome):
    first = 100 * run["id"]
    build = [("Set up job", "success", 0, 2, SETUP), ("Checkout", "success", 2, 4, CHECKOUT),
             ("Build the site", "success", 4, 31, BUILD), ("Upload site", "success", 31, 38, UPLOAD),
             ("Complete job", "success", 38, 39, CLEANUP)]
    if outcome == "in_progress":
        deploy = [("Set up job", "success", 40, 42, SETUP), ("Download site", "success", 42, 45, ["site.zip · 412 KB"]),
                  ("Publish preview", "in_progress", 45, None, PUBLISHING), ("Complete job", "queued", None, None, [])]
    else:
        deploy = [("Set up job", "success", 40, 42, SETUP), ("Download site", "success", 42, 45, ["site.zip · 412 KB"]),
                  ("Publish preview", "success", 45, 52, PUBLISH), ("Complete job", "success", 52, 53, CLEANUP)]
    return [design_job(first + 1, run, "Build site", "build", t0, build),
            design_job(first + 2, run, "Deploy preview", "deploy", t0, deploy)]


def seed_design():
    """The ten designed runs (newest first)."""
    now = time.time()
    quality, preview = DESIGN_FLOWS[:2]
    rows = [
        # id, number, workflow, title, branch, event, actor, pr, outcome, seconds ago
        (812, 12, preview, "Document the quickstart", "docs/quickstart", "pull_request", "ines", 17, "in_progress", 120),
        (807, 7, quality, "Exercise the retry-limit regression check", "fix/ci-regression", "pull_request", "maya", 18,
         "failure", 9 * 3600),
        (806, 6, quality, "Bound retries with exponential backoff (#16)", "main", "push", "rowan", None, "success",
         11 * 3600),
        (805, 5, quality, "Bound retries with exponential backoff", "fix/backoff", "pull_request", "rowan", 16, "success",
         26 * 3600),
        (811, 11, preview, "Bound retries with exponential backoff", "fix/backoff", "pull_request", "rowan", 16,
         "success", 26 * 3600 + 30),
        (804, 4, quality, "Bound retries with exponential backoff", "fix/backoff", "pull_request", "rowan", 16,
         "cancelled", 27 * 3600),
        (803, 3, quality, "Add a queue summary", "feat/queue", "pull_request", "maya", 15, "success", 50 * 3600),
        (810, 10, preview, "Add a queue summary", "feat/queue", "pull_request", "ines", 15, "skipped", 50 * 3600 + 20),
        (802, 2, quality, "Release 0.4.0", "main", "push", "rowan", None, "success", 3 * 86400),
        (809, 9, preview, "Preview deployment", "fix/retry", "workflow_dispatch", "maya", None, "success", 4 * 86400),
    ]
    runs, jobs, artifacts = [], {}, {}
    for run_id, number, flow, title, branch, event, actor, pr, outcome, ago in rows:
        t0 = int(now - ago)
        sha = hashlib.sha1(str(run_id).encode()).hexdigest()
        item = run(run_id, number, flow, title, branch, sha, event,
                   "in_progress" if outcome == "in_progress" else "completed",
                   None if outcome == "in_progress" else outcome, iso(t0), iso(t0), pulls=(pr,) if pr else ())
        item.update(actor={"login": actor}, triggering_actor={"login": actor}, run_started_at=iso(t0), pr=pr)
        if outcome == "skipped":
            listed = []
        elif flow is quality:
            listed = quality_jobs(item, t0, outcome)
        else:
            listed = preview_jobs(item, t0, outcome)
        ends = [job["completed_at"] for job in listed if job["completed_at"]]
        if outcome != "in_progress":
            item["updated_at"] = max(ends) if ends else iso(t0)
        runs.append(item)
        jobs[str(run_id)] = listed
    expires = iso(now + 30 * 86400)
    DESIGN_FILES[8701] = {"junit.xml": junit_xml(), "coverage.xml": coverage_xml()}
    DESIGN_FILES[8702] = {"coverage.json": coverage_json()}
    # GitLab keeps one archive per job: Lint's is its ruff report.
    DESIGN_FILES[8703] = {"ruff.json": b"[]\n"}
    artifacts["807"] = [{"id": 8701, "name": "test-report", "expired": False, "expires_at": expires},
                        {"id": 8702, "name": "coverage", "expired": False, "expires_at": expires}]
    # The manual preview of four days ago kept its site for three: listed, not downloadable.
    DESIGN_FILES[8901] = {"index.html": b"<!doctype html><title>orbit</title>\n"}
    artifacts["809"] = [{"id": 8901, "name": "site-preview", "expired": True, "expires_at": iso(now - 86400)}]
    # Release 0.4.0 passed on its second attempt: the first failed its tests.
    attempts = {}
    release = next(item for item in runs if item["id"] == 802)
    release["run_attempt"] = 2
    for listed in jobs["802"]:
        listed["run_attempt"] = 2
    start = int(now - 3 * 86400) - 600
    first = quality_jobs(dict(release, id=9802), start, "failure")
    for listed in first:
        listed.update(run_id=802, run_attempt=1)
    attempts["802/1"] = {"conclusion": "failure", "updated_at": iso(start + 7), "jobs": first}
    world = seed()
    world.update(runs=runs, jobs=jobs, attempts=attempts, artifacts=artifacts, workflows=DESIGN_FLOWS, next_run=900,
                 slow_cancel=True)
    return world


def design_world(state):
    world = state.get("actions")
    if world is None:
        world = state["actions"] = seed_design() if state.get("actions_seed") == "design" else seed()
        if state.get("actions_waiting"):
            waiting(world)
    return world


def waiting(world):
    """`state["actions_waiting"]`: a fork's run waiting for approval, second newest (the Git
    tutor's `A` lesson)."""
    now = time.time()
    flow = next(flow for flow in world["workflows"] if flow["name"] == "Quality checks")
    item = run(813, 13, flow, "Add retry jitter", "jitter", FORK_SHA, "pull_request", "completed",
               "action_required", iso(now - 600), iso(now - 600), owner="forker", pulls=(19,))
    item.update(actor={"login": "forker"}, triggering_actor={"login": "forker"}, run_started_at=iso(now - 600))
    world["runs"].insert(1, item)
    world["jobs"]["813"] = []


# --------------------------------------------------------------------------- GitLab and Gitea


GITLAB_STATUS = {"failure": "failed", "success": "success", "in_progress": "running", "cancelled": "canceled",
                 "skipped": "skipped", "queued": "pending", None: "running"}
GITLAB_SOURCE = {"pull_request": "merge_request_event", "push": "push", "workflow_dispatch": "web"}


def gitlab_user(name):
    return {"id": 2000 + sum(map(ord, name)), "username": name, "name": name.title(), "state": "active",
            "web_url": "https://gitlab.com/" + name}


def gitlab_trace(job):
    """The job's log as GitLab writes it: one `section_start`/`section_end` pair per step."""
    lines, step_lines, grouped = [], {}, False
    for line in DESIGN_LOGS.get(job["id"], "").lstrip("﻿").splitlines():
        stamp, _, text = line.partition(" ")
        step_lines.setdefault(stamp[:19], []).append(text)
    for step in job["steps"]:
        running = step["status"] == "in_progress"
        if not step["started_at"] or not step["completed_at"] and not running:
            continue
        start = int(time.mktime(time.strptime(step["started_at"], "%Y-%m-%dT%H:%M:%SZ")) - time.timezone)
        end = (int(time.mktime(time.strptime(step["completed_at"], "%Y-%m-%dT%H:%M:%SZ")) - time.timezone)
               if step["completed_at"] else 1 << 40)
        slug = re.sub(r"[^a-z0-9]+", "_", step["name"].lower()).strip("_")
        lines.append(f"\x1b[0Ksection_start:{start}:{slug}\r\x1b[0K{step['name']}")
        for stamp in sorted(step_lines):
            at = int(time.mktime(time.strptime(stamp, "%Y-%m-%dT%H:%M:%S")) - time.timezone)
            if start <= at < end:
                for text in step_lines[stamp]:
                    if text.startswith("##[group]"):
                        grouped = True
                        # GitLab echoes a script line; an action (`uses: x@v4`) has none.
                        if text.startswith("##[group]Run ") and "@" not in text:
                            lines.append("\x1b[32;1m$ " + text[len("##[group]Run "):] + "\x1b[0;m")
                    elif text.startswith("##[endgroup]"):
                        grouped = False
                    elif grouped:
                        continue
                    elif text.startswith("##[error]"):
                        lines.append("\x1b[31;1mERROR: Job failed: " + text[len("##[error]"):] + "\x1b[0;m")
                    elif not text.startswith("##["):
                        lines.append(text)
        if not running:
            lines.append(f"\x1b[0Ksection_end:{end}:{slug}\r\x1b[0K")
    return "\n".join(lines) + "\n"


def gitlab_world():
    """The designed runs as GitLab pipelines (newest first) and their jobs' traces."""
    world = seed_design()
    pipelines, traces = [], {}
    for index, item in enumerate(world["runs"]):
        jobs = []
        for job in world["jobs"][str(item["id"])]:
            status = GITLAB_STATUS[job["conclusion"] if job["status"] == "completed" else "in_progress"]
            seconds = None
            if job["started_at"] and job["completed_at"]:
                seconds = float(time.mktime(time.strptime(job["completed_at"], "%Y-%m-%dT%H:%M:%SZ"))
                                - time.mktime(time.strptime(job["started_at"], "%Y-%m-%dT%H:%M:%SZ")))
            body = {"id": job["id"], "name": job["name"], "stage": job["stage"], "status": status,
                    "allow_failure": False, "started_at": job["started_at"], "finished_at": job["completed_at"],
                    "duration": seconds, "web_url": f"https://gitlab.com/orbit/orbit/-/jobs/{job['id']}",
                    "retried": False, "failure_reason": "script_failure" if status == "failed" else None,
                    "user": gitlab_user(item["actor"]["login"])}
            if job["id"] in GITLAB_ARCHIVES:
                # The size of the archive `jobs/<id>/artifacts` serves, as GitLab reports it.
                size = len(artifact_zip(GITLAB_ARCHIVES[job["id"]]))
                body.update(artifacts_file={"filename": "artifacts.zip", "size": size},
                            artifacts=[{"file_type": "archive", "size": size, "filename": "artifacts.zip",
                                        "file_format": "zip"}],
                            artifacts_expire_at=world["artifacts"]["807"][0]["expires_at"])
            jobs.append(body)
            if job["status"] == "completed" or DESIGN_LOGS.get(job["id"]):
                # Keyed by the id as text: the world is kept as JSON between requests.
                traces[str(job["id"])] = gitlab_trace(job)
        pr = item.get("pr")
        status = GITLAB_STATUS[item["conclusion"] if item["status"] == "completed" else "in_progress"]
        started = time.mktime(time.strptime(item["run_started_at"], "%Y-%m-%dT%H:%M:%SZ"))
        updated = time.mktime(time.strptime(item["updated_at"], "%Y-%m-%dT%H:%M:%SZ"))
        pipelines.append({
            "id": 9000 + item["id"], "iid": 400 - index, "project_id": 4242, "sha": item["head_sha"],
            "ref": f"refs/merge-requests/{pr}/head" if pr else item["head_branch"], "status": status,
            "source": GITLAB_SOURCE[item["event"]], "created_at": item["created_at"],
            "updated_at": item["updated_at"],
            "duration": int(updated - started) if item["status"] == "completed" and jobs else None,
            "web_url": f"https://gitlab.com/orbit/orbit/-/pipelines/{9000 + item['id']}",
            "name": item["display_title"], "user": gitlab_user(item["actor"]["login"]), "jobs": jobs})
    return pipelines, traces


FORGEJO_STATUS = {"failure": "failure", "success": "success", "cancelled": "cancelled", "skipped": "skipped"}


def gitea_answer(state, request, world_name):
    """`repos/orbit/orbit/actions/*` from the designed world, in Gitea's GitHub-shaped form or
    Forgejo 17's own (`index_in_repo`, one status word, bare lists); None to fall through."""
    rest = request.rest.split("?", 1)[0]
    listed = re.fullmatch(r"repos/orbit/orbit/contents/(\.(?:forgejo|gitea|github))/workflows", rest)
    if listed and request.method == "GET":
        # The workflow files, as the contents API lists a directory: Forgejo keeps them in
        # `.forgejo/`, Gitea in `.gitea/`; the other directories are absent.
        if listed.group(1) != (".forgejo" if world_name == "forgejo" else ".gitea"):
            return 404, {"message": "GetContentsOrList", "url": "", "errors": None}
        return 200, [{"name": name, "path": f"{listed.group(1)}/workflows/{name}", "type": "file", "size": 600,
                      "sha": hashlib.sha1(name.encode()).hexdigest()}
                     for name in (flow["path"].rsplit("/", 1)[1] for flow in DESIGN_FLOWS)]
    match = re.fullmatch(r"repos/orbit/orbit/actions/(.*)", rest)
    if not match or request.method != "GET":
        return None
    world = state.get("actions") or state.setdefault("actions", seed_design())
    tail, forgejo = match.group(1), world_name == "forgejo"
    host = request.headers.get("host", "127.0.0.1")
    web = f"https://{request.host}/orbit/orbit"

    def path(item):
        flow = next(flow for flow in DESIGN_FLOWS if flow["id"] == item["workflow_id"])
        return flow["path"].rsplit("/", 1)[1]

    def word(item):
        if item["status"] != "completed":
            return "running"
        return FORGEJO_STATUS.get(item["conclusion"], "unknown")

    def run_body(item):
        stopped = item["updated_at"] if item["status"] == "completed" else None
        if forgejo:
            pr = item.get("pr")
            started = time.mktime(time.strptime(item["run_started_at"], "%Y-%m-%dT%H:%M:%SZ"))
            ended = time.mktime(time.strptime(item["updated_at"], "%Y-%m-%dT%H:%M:%SZ"))
            return {"id": item["id"], "title": item["display_title"], "workflow_id": path(item),
                    "index_in_repo": item["run_number"], "trigger_user": item["actor"],
                    "prettyref": f"#{pr}" if pr else item["head_branch"], "commit_sha": item["head_sha"],
                    "is_fork_pull_request": False, "need_approval": False, "event": item["event"],
                    "trigger_event": item["event"], "status": word(item), "started": item["run_started_at"],
                    "stopped": stopped or "0001-01-01T00:00:00Z", "created": item["created_at"],
                    "updated": item["updated_at"],
                    "duration": int((ended - started) * 1e9) if stopped else 0,
                    "html_url": f"{web}/actions/runs/{item['run_number']}"}
        return {"id": item["id"], "run_number": item["run_number"], "display_title": item["display_title"],
                "event": item["event"], "status": item["status"], "conclusion": item["conclusion"],
                "head_branch": item["head_branch"], "head_sha": item["head_sha"],
                "path": f"{path(item)}@refs/heads/{item['head_branch']}", "run_attempt": 1,
                "started_at": item["run_started_at"], "completed_at": stopped, "created_at": item["created_at"],
                "html_url": f"{web}/actions/runs/{item['run_number']}", "actor": item["actor"],
                "trigger_actor": item["actor"],
                "pull_requests": [{"number": item["pr"]}] if item.get("pr") else []}

    def job_body(item, job):
        if forgejo:
            steps = [{"number": step["number"] - 1, "name": step["name"],
                      "status": "running" if step["status"] == "in_progress" else
                      "waiting" if step["status"] == "queued" else FORGEJO_STATUS.get(step["conclusion"], "unknown"),
                      "started": step["started_at"] or "0001-01-01T00:00:00Z",
                      "stopped": step["completed_at"] or "0001-01-01T00:00:00Z"} for step in job["steps"]]
            return {"id": job["id"], "run_id": item["id"], "attempt": 1, "name": job["name"],
                    "task_id": job["id"] + 1, "status": word(job), "steps": steps,
                    "html_url": f"{web}/actions/runs/{item['run_number']}/jobs/0/attempt/1"}
        return {"id": job["id"], "run_id": item["id"], "name": job["name"], "status": job["status"],
                "conclusion": job["conclusion"], "head_branch": item["head_branch"], "head_sha": item["head_sha"],
                "started_at": job["started_at"], "completed_at": job["completed_at"], "runner_name": "runner-1",
                "labels": ["ubuntu-latest"], "steps": job["steps"], "run_attempt": 1,
                "html_url": f"{web}/actions/runs/{item['run_number']}/jobs/0"}

    query = request.query
    if tail == "workflows" and not forgejo:
        flows = [{"id": path_name, "name": flow["name"], "path": ".gitea/workflows/" + path_name,
                  "state": flow["state"]}
                 for flow in DESIGN_FLOWS for path_name in [flow["path"].rsplit("/", 1)[1]]]
        return 200, {"total_count": len(flows), "workflows": flows}
    if tail == "runs":
        if query.get("page") not in (None, "1"):
            return 200, {"total_count": len(world["runs"]), "workflow_runs": []}
        chosen = [item for item in world["runs"]
                  if (not query.get("branch") or item["head_branch"] == query["branch"])
                  and (not query.get("event") or item["event"] == query["event"])]
        return 200, {"total_count": len(chosen), "workflow_runs": [run_body(item) for item in chosen]}
    found = re.fullmatch(r"runs/(\d+)(/jobs|/artifacts)?", tail)
    if found:
        item = next((item for item in world["runs"] if item["id"] == int(found.group(1))), None)
        if item is None:
            return 404, {"message": "run not found"}
        if found.group(2) == "/jobs":
            jobs = [job_body(item, job) for job in world["jobs"].get(str(item["id"]), [])]
            return 200, jobs if forgejo else {"total_count": len(jobs), "jobs": jobs}
        if found.group(2) == "/artifacts":
            items = [{"id": artifact["id"], "name": artifact["name"],
                      "size_in_bytes": len(artifact_zip(artifact["id"])), "expired": artifact["expired"],
                      "created_at": item["updated_at"], "expires_at": artifact["expires_at"],
                      "archive_download_url": f"https://{request.host}/api/v1/repos/orbit/orbit/actions/artifacts/"
                                              f"{artifact['id']}/zip",
                      "workflow_run": {"id": item["id"], "head_sha": item["head_sha"]}}
                     for artifact in world["artifacts"].get(str(item["id"]), [])]
            return 200, items if forgejo else {"total_count": len(items), "artifacts": items}
        return 200, run_body(item)
    found = re.fullmatch(r"jobs/(\d+)(/logs)?", tail)
    if found:
        job_id = int(found.group(1))
        for item in world["runs"]:
            for job in world["jobs"].get(str(item["id"]), []):
                if job["id"] == job_id:
                    if found.group(2):
                        return 200, DESIGN_LOGS.get(job_id, "")
                    return 200, job_body(item, job)
        return 404, {"message": "job not found"}
    _ = host
    return None
