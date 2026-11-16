#!/usr/bin/env python3
"""Fake GitHub REST v3 + GraphQL server for forge tests. Never touches the network.

    server.py --state PATH [--world legacy|inbox|recorded|gitlab|gitea|forgejo|actions] [--fixtures DIR]

Prints the port on stdout, then serves `http://127.0.0.1:PORT/{host}/api/v3/...` and
`http://127.0.0.1:PORT/{host}/api/graphql` until killed. The logical forge host is the first path
segment, so one server stands in for any number of hosts (`DUSTY_FORGE_API_BASE` carries a
`{host}` placeholder).

The world lives in a JSON state file that tests read and rewrite while the server runs. Every
request is appended to `state["requests"]` as
`{transport, method, host, path, query, operation, variables, body, auth, call, gh_argv}`.
`call` is the gh-CLI argv the legacy `github_cli.py` fixture would have seen for the same logical
read or write (None for secondary requests such as a run's jobs page), so the ported screen tests
keep their call-log assertions.

Two synthetic worlds are ported from the old fixtures: `legacy` (tests/fixtures/github_cli.py)
and `inbox` (tests/fixtures/github_inbox_cli.py). `--fixtures` adds a recorded layer: a request
whose `METHOD /path?sorted-query` key matches an entry of `requests.json` is answered from the
recording (in any world; the `recorded` world answers nothing else).

Direct-HTTP requests must carry `Bearer|token <FAKE_TOKEN>` (else 401); requests forwarded by the
fake gh (`X-Fake-Transport: gh`) are trusted, as gh attaches its own sign-in. The token is never
written to the state file.

F2 additions (discovery and credentials): GitLab `/{host}/api/v4/...` and Gitea/Forgejo
`/{host}/api/v1/...` paths, forwarded by the fake glab and tea (`X-Fake-Transport: glab|tea`);
`PRIVATE-TOKEN` auth and the extra accepted tokens of `state["tokens"]`; the unauthenticated
version probes (`api/v1/version`, `api/forgejo/v1/version`, `api/v4/version`, `api/v3/meta`)
answered from `state["probes"][host][path] = {status, body}` (404 otherwise), before any auth
check; `GET user` on those APIs answers `state["viewer"]` (default `rowan`); anything else on them
comes from `state["answers"]["METHOD path"] = {status, body, headers}` or is 404.
"""
import argparse
import base64
import fcntl
import hashlib
import json
import os
import socket
import pathlib
import re
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import world_gitlab  # noqa: E402  (F5: `--world gitlab`)
import world_gitea  # noqa: E402  (F4: `--world gitea|forgejo`)
import world_actions  # noqa: E402  (F6: `--world actions`)
import world_conversation  # noqa: E402  (`state["conversation"]`: a real-looking PR and issue)

FAKE_TOKEN = "ghp_fixture0123456789abcdefABCDEF0123"


def blob_sha(text):
    """Git's blob id for `text`, as the contents API names a file's version."""
    data = text.encode()
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


class StateLock:
    """Who may read-modify-write the state file: one handler thread here at a time, and tests
    that rewrite it through `forge_fake::write_state`, which take the same advisory lock on
    `<state>.lock`. Without it a test's rewrite landing while a request is answered was lost."""

    def __init__(self):
        self.thread = threading.Lock()
        self.file = None

    def __enter__(self):
        self.thread.acquire()
        self.file = open(ARGS.state + ".lock", "a")
        fcntl.flock(self.file, fcntl.LOCK_EX)
        return self

    def __exit__(self, *_):
        fcntl.flock(self.file, fcntl.LOCK_UN)
        self.file.close()
        self.file = None
        self.thread.release()


LOCK = StateLock()
# Concurrent API requests being answered, and the most ever seen (`GET /_fake/stats`).
FLIGHT = {"now": 0, "max": 0}
FLIGHT_LOCK = threading.Lock()
ARGS = None
RECORDED = {}

ISSUE_LIST_FIELDS = ["number", "title", "body", "state", "author", "labels", "updatedAt", "comments"]
PR_LIST_FIELDS = ISSUE_LIST_FIELDS + ["isDraft", "reviewDecision", "statusCheckRollup", "changedFiles"]
SEARCH_FIELDS = ["number", "title", "body", "state", "author", "labels", "updatedAt", "commentsCount", "repository", "url"]
ISSUE_VIEW_FIELDS = ["id", "number", "title", "body", "state", "author", "createdAt", "updatedAt", "assignees",
                     "labels", "milestone", "projectItems", "closedByPullRequestsReferences"]
PR_VIEW_FIELDS = ["id", "number", "title", "body", "state", "author", "createdAt", "assignees", "labels", "milestone",
                  "projectItems", "baseRefName", "headRefName", "headRefOid", "isDraft", "changedFiles", "commits",
                  "files", "reviewDecision", "mergeStateStatus", "mergeable", "reviews", "statusCheckRollup",
                  "closingIssuesReferences"]


class Reply(Exception):
    """An HTTP answer raised from deep inside a handler."""

    def __init__(self, status, body, headers=None):
        super().__init__(status)
        self.status, self.body, self.headers = status, body, headers or {}


def fail_rest(status, message):
    raise Reply(status, {"message": message, "documentation_url": "https://docs.github.com/rest"})


def fail_graphql(message, kind=None):
    error = {"message": message}
    if kind:
        error["type"] = kind
    raise Reply(200, {"data": None, "errors": [error]})


# --------------------------------------------------------------------------- state


# The text `load_state` last read, so `save_state` can tell that a test rewrote the file meanwhile.
LOADED = {"text": None}


def read_state_text(path):
    """The whole state text. A test's plain `std::fs::write` truncates before it writes, so an
    empty or unparseable read is a rewrite in progress: read again once it is whole."""
    deadline = time.monotonic() + 3
    while True:
        exists = path.exists()
        try:
            text = path.read_text() if exists else ""
            if text.strip():
                json.loads(text)
                return text
            if not exists or time.monotonic() > deadline:
                return text
        except (json.JSONDecodeError, UnicodeDecodeError):
            if time.monotonic() > deadline:
                raise
        time.sleep(0.01)


def load_state():
    text = read_state_text(pathlib.Path(ARGS.state))
    LOADED["text"] = text
    return json.loads(text) if text.strip() else {}


def merge_state(base, ours, theirs):
    """A three-way merge by top-level key of this server's changes since `base` (`ours`) into a
    test's rewrite (`theirs`). Where both changed a key the test's rewrite wins, except that the
    request log only ever gains the requests logged since."""
    missing = object()
    merged = dict(theirs)
    for key in set(base) | set(ours):
        was, mine, its = base.get(key, missing), ours.get(key, missing), theirs.get(key, missing)
        if mine == was:
            continue
        if key == "requests":
            seen = was if isinstance(was, list) else []
            logged = mine[len(seen):] if isinstance(mine, list) else []
            merged[key] = (its if isinstance(its, list) else []) + logged
        elif its == was:
            if mine is missing:
                merged.pop(key, None)
            else:
                merged[key] = mine
    return merged


def save_state(state):
    path = pathlib.Path(ARGS.state)
    temporary = path.with_name(path.name + ".server-tmp")
    while True:
        text = json.dumps(state)
        temporary.write_text(text)
        current = read_state_text(path)
        loaded = LOADED["text"]
        if loaded is None or current == loaded or not current.strip():
            break
        # A test rewrote the file without the lock while this request was answered: keep its
        # rewrite, apply this request's changes on top, and check again.
        base = json.loads(loaded) if loaded.strip() else {}
        state = merge_state(base, state, json.loads(current))
        LOADED["text"] = current
    temporary.replace(path)
    LOADED["text"] = text


# --------------------------------------------------------------------------- requests


class Request:
    def __init__(self, handler):
        parsed = urllib.parse.urlsplit(handler.path)
        self.method = handler.command
        self.raw_path = parsed.path
        self.query = dict(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
        self.query_string = parsed.query
        self.headers = {key.lower(): value for key, value in handler.headers.items()}
        length = int(self.headers.get("content-length") or 0)
        self.body_bytes = handler.rfile.read(length) if length else b""
        marked = self.headers.get("x-fake-transport")
        self.transport = marked if marked in ("gh", "glab", "tea") else "http"
        argv = self.headers.get("x-fake-gh-argv")
        self.gh_argv = json.loads(argv) if argv else None
        parts = parsed.path.lstrip("/").split("/", 1)
        self.host = urllib.parse.unquote(parts[0]) if parts else ""
        rest = parts[1] if len(parts) > 1 else ""
        self.full = rest
        self.api = "github"
        for prefix, api in (("api/v4/", "gitlab"), ("api/v1/", "gitea")):
            if rest.startswith(prefix):
                self.api = api
        self.graphql = rest == "api/graphql"
        if self.api != "github":
            self.rest = rest.split("/", 2)[2]
        else:
            self.rest = rest[len("api/v3/"):] if rest.startswith("api/v3/") else (None if self.graphql else rest)
        self.json = None
        if self.body_bytes:
            try:
                self.json = json.loads(self.body_bytes)
            except ValueError:
                self.json = None
        self.document = (self.json or {}).get("query", "") if self.graphql else ""
        self.variables = (self.json or {}).get("variables") or {} if self.graphql else {}
        match = re.match(r"\s*(query|mutation)\s+(\w+)", self.document)
        self.operation = match.group(2) if match else None
        self.mutation = bool(match and match.group(1) == "mutation")
        self.accept = self.headers.get("accept", "")
        auth = self.headers.get("authorization", "")
        if self.headers.get("private-token"):
            auth = "Bearer " + self.headers["private-token"]
        self.token = auth.split(" ", 1)[1] if " " in auth else ""
        if self.transport != "http":
            self.auth = self.transport
        elif not auth:
            self.auth = "missing"
        elif auth in ("Bearer " + FAKE_TOKEN, "token " + FAKE_TOKEN):
            self.auth = "valid"
        else:
            self.auth = "invalid"
        self.repo = None
        self.tail = None
        if self.rest is not None:
            match = re.match(r"repos/([^/]+)/([^/]+)/?(.*)$", self.rest)
            if match:
                self.repo = match.group(1) + "/" + match.group(2)
                self.tail = match.group(3)
        if self.graphql and "owner" in self.variables and "name" in self.variables:
            self.repo = self.variables["owner"] + "/" + self.variables["name"]

    def path_and_query(self):
        return self.rest + ("?" + self.query_string if self.query_string else "")

    def wants_diff(self):
        return "diff" in self.accept


class PorcelainRequest:
    """The few request fields a world reads while answering a porcelain gh command."""

    def __init__(self, host):
        self.host, self.repo, self.variables = host or "github.com", None, {}


# --------------------------------------------------------------------------- search queries


def parse_search(q):
    """`repo:o/r is:issue is:open <user terms> sort:created-desc` → (repo, kind, state, terms)."""
    terms = q.strip()
    repo = None
    match = re.match(r"repo:(\S+)\s*", terms)
    if match:
        repo, terms = match.group(1), terms[match.end():]
    kind = "issue"
    match = re.match(r"is:(issue|pr)\b\s*", terms)
    if match:
        kind, terms = match.group(1), terms[match.end():]
    state = "all"
    for word, name in (("is:closed is:unmerged", "closed"), ("is:open", "open"), ("is:closed", "closed"),
                       ("is:merged", "merged")):
        if terms == word or terms.startswith(word + " "):
            state, terms = name, terms[len(word):]
            break
    terms = terms.strip()
    # The provider appends its default sort only when the user wrote none.
    default = "sort:created-desc" if repo else "sort:updated-desc"
    if terms == default or terms.endswith(" " + default):
        terms = terms[:-len(default)].strip()
    return repo, kind, state, terms


def kind_of(state, host, repo, number, default="issue"):
    """REST comments/labels share `issues/N` for PRs; the last list row or detail read of that
    number names the kind (gh's porcelain named it in the command)."""
    return state.get("_kinds", {}).get(f"{host}/{repo}#{number}", default)


def remember_kind(state, host, repo, number, kind):
    state.setdefault("_kinds", {})[f"{host}/{repo}#{number}"] = kind


# --------------------------------------------------------------------------- gh-shaped → API-shaped


def is_context(check):
    """A StatusContext: gh's rollup gives it `context`/`state`, a CheckRun `name`/`status`."""
    return "context" in check or ("state" in check and "status" not in check and "conclusion" not in check)


def rollup_word(check):
    if is_context(check):
        return (check.get("state") or "PENDING").upper()
    return (check.get("conclusion") or check.get("status") or "PENDING").upper()


FAILED = {"FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE", "CANCELLED", "ACTION_REQUIRED"}
PENDING = {"PENDING", "IN_PROGRESS", "QUEUED", "EXPECTED", "WAITING", "REQUESTED"}


def rollup_state(checks):
    if not checks:
        return None
    words = [rollup_word(check) for check in checks]
    if any(word in FAILED for word in words):
        return "FAILURE"
    if any(word in PENDING for word in words):
        return "PENDING"
    return "SUCCESS"


def counts(checks, contexts):
    tally = {}
    for check in checks:
        if is_context(check) == contexts:
            word = rollup_word(check)
            tally[word] = tally.get(word, 0) + 1
    return [{"state": word, "count": count} for word, count in tally.items()]


def gql_check(check):
    if is_context(check):
        return {"__typename": "StatusContext", "context": check.get("context", ""), "state": check.get("state", "PENDING"),
                "targetUrl": check.get("targetUrl"), "createdAt": check.get("startedAt")}
    match = re.search(r"/runs/(\d+)", check.get("detailsUrl") or "")
    conclusion = check.get("conclusion") or None
    return {"__typename": "CheckRun", "name": check.get("name", ""),
            "status": check.get("status") or ("COMPLETED" if conclusion else "IN_PROGRESS"),
            "conclusion": conclusion, "startedAt": check.get("startedAt"), "completedAt": check.get("completedAt"),
            "detailsUrl": check.get("detailsUrl"),
            "checkSuite": {"workflowRun": {"databaseId": int(match.group(1)),
                                           "workflow": {"name": check.get("workflowName", "")}} if match else None}}


def link_nodes(entries, host, repo, kind):
    path = "issues" if kind == "issue" else "pull"
    return {"nodes": [{"number": entry.get("number"), "title": entry.get("title", ""),
                       "url": entry.get("url") or f"https://{host}/{repo}/{path}/{entry.get('number')}",
                       "repository": {"nameWithOwner": repo}} for entry in entries]}


def gql_item(entry, kind, host, repo, detail):
    """One gh `--json` item → the GraphQL node the GitHub provider's documents select."""
    number = entry["number"]
    comments = entry.get("comments")
    total = len(comments) if isinstance(comments, list) else entry.get("commentsCount", 0)
    node = {"__typename": "Issue" if kind == "issue" else "PullRequest",
            "id": entry.get("id", f"{kind.upper()}_NODE_{number}"), "number": number,
            "title": entry.get("title", ""), "state": str(entry.get("state", "OPEN")).upper(),
            "url": entry.get("url") or f"https://{host}/{repo}/{'issues' if kind == 'issue' else 'pull'}/{number}",
            "createdAt": entry.get("createdAt", ""), "updatedAt": entry.get("updatedAt") or entry.get("createdAt", ""),
            "author": entry.get("author") or {"login": "ghost"},
            "labels": {"nodes": entry.get("labels", [])}, "comments": {"totalCount": total},
            "repository": {"nameWithOwner": (entry.get("repository") or {}).get("nameWithOwner") or repo}}
    for key in ("body", "stateReason"):
        if key in entry:
            node[key] = entry[key]
    files = entry.get("files", [])
    checks = entry.get("statusCheckRollup", [])
    if kind == "pr":
        node.update(isDraft=bool(entry.get("isDraft", False)), reviewDecision=entry.get("reviewDecision") or None,
                    additions=sum(file.get("additions", 0) for file in files),
                    deletions=sum(file.get("deletions", 0) for file in files),
                    changedFiles=entry.get("changedFiles", len(files)))
        if not detail:
            node["headRefName"] = entry.get("headRefName", "")
            # Each reviewer's latest approval or change request, and the requests still open.
            latest = {}
            for review in entry.get("reviews", []):
                if review.get("state") in ("APPROVED", "CHANGES_REQUESTED"):
                    latest[(review.get("author") or {}).get("login", "")] = review["state"]
            node["latestOpinionatedReviews"] = {"nodes": [{"state": state} for state in latest.values()]}
            node["reviewRequests"] = {"totalCount": len(entry.get("reviewRequests", []))}
            node["commits"] = {"nodes": [{"commit": {"statusCheckRollup": None if not checks else {
                "state": rollup_state(checks),
                "contexts": {"checkRunCountsByState": counts(checks, False),
                             "statusContextCountsByState": counts(checks, True)}}}}]}
    if not detail:
        # The linked pull requests (an issue) or closing issues (a pull request) a list row counts.
        links = "closedByPullRequestsReferences" if kind == "issue" else "closingIssuesReferences"
        node[links] = {"totalCount": len(entry.get(links, []))}
        return node
    milestone = entry.get("milestone")
    node.update(assignees={"nodes": entry.get("assignees", [])},
                milestone=dict({"number": 1, "state": "OPEN"}, **milestone) if milestone else None,
                projectItems={"nodes": [{"project": {"title": item.get("title", "")}}
                                        for item in entry.get("projectItems", [])]},
                viewerSubscription=entry.get("viewerSubscription"))
    if kind == "issue":
        node["closedByPullRequestsReferences"] = link_nodes(entry.get("closedByPullRequestsReferences", []),
                                                            host, repo, "pr")
        return node
    node.update(baseRefName=entry.get("baseRefName", "main"), headRefName=entry.get("headRefName", ""),
                headRefOid=entry.get("headRefOid", ""), baseRefOid=entry.get("baseRefOid", "b" * 40),
                headRepository={"nameWithOwner": repo}, mergeable=entry.get("mergeable", "UNKNOWN"),
                mergeStateStatus=entry.get("mergeStateStatus", "UNKNOWN"),
                reviewRequests={"nodes": [{"requestedReviewer": {"__typename": "User", "login": login}}
                                          for login in entry.get("reviewRequests", [])]},
                reviews={"nodes": [dict({"id": f"REVIEW_{index}", "submittedAt": entry.get("createdAt", "")}, **review)
                                   for index, review in enumerate(entry.get("reviews", []))]},
                commits={"nodes": [{"commit": {"oid": commit.get("oid", ""),
                                               "messageHeadline": commit.get("messageHeadline", ""),
                                               "messageBody": commit.get("messageBody", ""),
                                               "committedDate": commit.get("committedDate", ""),
                                               "author": {"user": (commit.get("authors") or [None])[0]}}}
                                   for commit in entry.get("commits", [])]},
                files={"pageInfo": {"hasNextPage": False, "endCursor": None},
                       "nodes": [{"path": file["path"], "additions": file.get("additions", 0),
                                  "deletions": file.get("deletions", 0),
                                  "changeType": file.get("changeType", "MODIFIED")} for file in files]},
                closingIssuesReferences=link_nodes(entry.get("closingIssuesReferences", []), host, repo, "issue"),
                latest={"nodes": [{"commit": {"statusCheckRollup": None if not checks else {
                    "state": rollup_state(checks), "contexts": {"nodes": [gql_check(check) for check in checks]}}}}]})
    return node


def rest_run(run, host, repo):
    conclusion = run.get("conclusion") or None
    return {"id": run["databaseId"], "run_number": run.get("number", run["databaseId"]),
            "name": run.get("workflowName", ""), "display_title": run.get("displayTitle", ""),
            "workflow_id": run.get("workflowDatabaseId"), "path": run.get("path", ""),
            "status": run.get("status", "queued"), "conclusion": conclusion,
            "head_branch": run.get("headBranch", ""), "head_sha": run.get("headSha", ""),
            "event": run.get("event", ""), "created_at": run.get("createdAt", "2026-11-08T10:00:00Z"),
            "run_attempt": run.get("attempt", 1),
            "html_url": f"https://{host}/{repo}/actions/runs/{run['databaseId']}", "pull_requests": [],
            # Who started it, when the world says (r16 notif-actions-11): rows and the run line name them.
            **({"triggering_actor": {"login": run["actor"]}} if run.get("actor") else {})}


def rest_job(job, run, host, repo):
    return {"id": job["databaseId"], "run_id": run["databaseId"], "name": job.get("name", ""),
            "status": job.get("status", "queued"), "conclusion": job.get("conclusion") or None,
            "started_at": job.get("startedAt"), "completed_at": job.get("completedAt"),
            "html_url": f"https://{host}/{repo}/actions/runs/{run['databaseId']}/job/{job['databaseId']}",
            "steps": [{"number": step.get("number", 0), "name": step.get("name", ""),
                       "status": step.get("status", "queued"), "conclusion": step.get("conclusion") or None,
                       "started_at": step.get("startedAt"), "completed_at": step.get("completedAt")}
                      for step in job.get("steps", [])]}


def strip_gh_columns(text):
    """`gh run view --log` lines are `job<TAB>step<TAB>line`; the REST log is the bare lines."""
    lines = []
    for line in text.splitlines(keepends=True):
        parts = line.split("\t", 2)
        lines.append(parts[2] if len(parts) == 3 else line)
    return "".join(lines)


# --------------------------------------------------------------------------- the legacy world


DEFAULT_DIFF = ('diff --git a/example.rs b/example.rs\nindex 1111111..2222222 100644\n--- a/example.rs\n'
                '+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\n'
                'diff --git a/src/nested/other.rs b/src/nested/other.rs\n--- /dev/null\n+++ b/src/nested/other.rs\n'
                '@@ -0,0 +1 @@\n+fn second() {}\n')
COMMIT_DIFF = ('diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n'
               '-fn before() {}\n+fn after() {}\n')
DEFAULT_LOG = ("test (linux)\tUNKNOWN STEP\t﻿2026-11-08T09:59:58.100Z Current runner version: fixture\n"
               "test (linux)\tUNKNOWN STEP\t2026-11-08T10:00:00.200Z \x1b[32mall native checks passed\x1b[0m\n")
WORKFLOW_YAML = """name: Native CI
on:
  workflow_dispatch:
    inputs:
      target:
        description: Target environment
        type: choice
        required: true
        default: staging
        options: [staging, production]
      dry_run:
        type: boolean
        default: true
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build
  test:
    needs: [build]
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - run: cargo test
"""
WORKFLOWS = [{"id": 31, "name": "Native CI", "path": ".github/workflows/native.yml", "state": "active"}]


class Legacy:
    """`tests/fixtures/github_cli.py`'s world, answered as REST + GraphQL."""

    def __init__(self, state, request):
        if state.get("conversation") and not state.get("conversation_seeded"):
            world_conversation.github(state)
            state["conversation_seeded"] = True
        self.state, self.request = state, request
        self.host = request.host
        self.item = {"id": "NODE_7", "number": 7, "title": "Preserve keyboard drafts",
                     "body": "A real native issue layout.\n\nThe description has multiple paragraphs.",
                     "state": state.get("issue_state", "OPEN"), "author": {"login": "alice"},
                     "createdAt": "2026-11-08T10:00:00Z",
                     "comments": [{"body": body} for body in state.get("comments", [])],
                     "labels": [{"name": "bug"}], "assignees": [{"login": "bob"}], "milestone": {"title": "v1"},
                     "projectItems": [{"title": "Native editor"}],
                     "closedByPullRequestsReferences": [{"number": 9, "title": "Native review"}]}
        self.pr = dict(self.item, id="PR_NODE_9", number=9, title="Native review", baseRefName="main",
                       headRefName="feature/native", headRefOid="a" * 40, isDraft=False, mergeStateStatus="CLEAN",
                       mergeable="MERGEABLE", reviewDecision="APPROVED",
                       reviews=[{"author": {"login": "reviewer"}, "state": "APPROVED",
                                 "body": "Reviewed the actual change."}],
                       commits=[{"oid": "a" * 40, "messageHeadline": "Keep drafts", "messageBody": "",
                                 "committedDate": "2026-11-08"}],
                       changedFiles=2, files=[{"path": "example.rs", "additions": 1, "deletions": 1},
                                              {"path": "src/nested/other.rs", "additions": 1, "deletions": 0}],
                       statusCheckRollup=[{"name": "test (linux)", "conclusion": "SUCCESS",
                                           "detailsUrl": "https://github.com/example/demo/actions/runs/91/job/42"}],
                       closingIssuesReferences=[{"number": 7, "title": self.item["title"]}])
        run = {"databaseId": 91, "displayTitle": "Validate native workspace", "workflowName": "Native CI",
               "workflowDatabaseId": 31, "headBranch": "main", "headSha": "a" * 40, "event": "push", "attempt": 2,
               "status": "completed", "conclusion": "success", "path": ".github/workflows/native.yml",
               "jobs": [{"databaseId": 41, "name": "build", "status": "completed", "conclusion": "success",
                         "steps": [{"name": "Build", "number": 1, "status": "completed", "conclusion": "success"}]},
                        {"databaseId": 42, "name": "test (linux)", "status": "completed", "conclusion": "success",
                         "steps": [{"name": "Check", "number": 1, "status": "completed", "conclusion": "success"}]},
                        {"databaseId": 43, "name": "test (windows)", "status": "completed", "conclusion": "success",
                         "steps": []}]}
        self.run = json.loads(json.dumps(state.get("run", run)))
        # A world with several runs (`runs`) answers each by its id; `run` stays the fallback
        # for every other id, as a world with one run expects.
        self.runs = {entry["databaseId"]: entry for entry in json.loads(json.dumps(state.get("runs", [])))}
        self.workflows = state.get("workflows", WORKFLOWS)

    def run_by_id(self, run_id):
        return self.runs.get(run_id, self.run)

    def all_runs(self):
        if not self.runs:
            return [self.run]
        runs = dict(self.runs)
        runs.setdefault(self.run["databaseId"], self.run)
        return sorted(runs.values(), key=lambda entry: entry.get("createdAt", ""), reverse=True)

    def repo(self):
        return self.request.repo or self.state.get("repository", "example/demo")

    def entries(self, kind):
        default = self.item if kind == "issue" else self.pr
        return [dict(default, **entry) for entry in self.state.get(kind + "s", [default])]

    def find(self, kind, number):
        default = self.item if kind == "issue" else self.pr
        return next((entry for entry in self.entries(kind) if entry["number"] == number), default)

    def refresh_run(self, counted):
        if counted:
            self.state["run_views"] = self.state.get("run_views", 0) + 1
        if self.state.get("run_views", 0) < 2:
            self.run.update(status="in_progress", conclusion="")
            if len(self.run.get("jobs", [])) > 1:
                self.run["jobs"][1].update(status="in_progress", conclusion="")
        for entry in [self.run, *self.runs.values()]:
            for job in entry.get("jobs", []):
                for step in job.get("steps", []):
                    step.setdefault("startedAt", "2026-11-08T10:00:00Z")
                    step.setdefault("completedAt", "2026-11-08T10:00:01Z")

    # ---- GraphQL

    def search(self, kind, state_name, terms, scoped):
        if self.state.get("empty"):
            return []
        entries = self.entries(kind)
        overrides = self.state.get(kind + "_queries", {}).get(terms)
        default = self.item if kind == "issue" else self.pr
        if overrides is not None:
            entries = [dict(default, **entry) for entry in overrides]
        if state_name != "all":
            entries = [entry for entry in entries if str(entry["state"]).upper() == state_name.upper()]
        if self.state.get("filter_terms"):
            # Screenshot worlds: honour `label:`, `-label:`, `draft:` and words in the title.
            for term in terms.split():
                negate, word = term.startswith("-"), term.lstrip("-")
                name, _, value = word.partition(":")
                if name == "label" and value:
                    keep = lambda entry: any(label.get("name") == value for label in entry.get("labels", []))
                elif name == "draft" and value:
                    keep = lambda entry: bool(entry.get("isDraft")) == (value == "true")
                elif name in ("review-requested", "author") and value:
                    who = self.state.get("viewer", "rowan") if value == "@me" else value
                    if name == "author":
                        keep = lambda entry, who=who: (entry.get("author") or {}).get("login") == who
                    else:
                        keep = lambda entry, who=who: who in entry.get("reviewRequests", [])
                elif ":" in word:
                    continue
                else:
                    keep = lambda entry: word.lower() in str(entry.get("title", "")).lower()
                entries = [entry for entry in entries if keep(entry) != negate]
        if self.state.get("sort_search", self.state.get("filter_terms")):
            # Screenshot worlds (filter_terms, or sort_search): order rows as GitHub's search
            # does, by the `sort:` term, else its default (created-desc in a repository,
            # updated-desc across them).
            order = next((term[5:] for term in terms.split() if term.startswith("sort:")),
                         "created-desc" if scoped else "updated-desc")
            field, _, direction = order.partition("-")
            key = {"created": lambda entry: entry.get("createdAt", ""),
                   "updated": lambda entry: entry.get("updatedAt") or entry.get("createdAt", ""),
                   "comments": lambda entry: len(entry.get("comments") or [])
                   if isinstance(entry.get("comments"), list) else entry.get("comments") or 0}.get(field)
            if key:
                entries = sorted(entries, key=key, reverse=direction != "asc")
        omitted = self.state.get("list_omit_fields", [])
        return [{key: value for key, value in entry.items() if key not in omitted} for entry in entries]

    def graphql(self):
        request, variables = self.request, self.request.variables
        operation = request.operation
        if operation in ("IssueList", "PullRequestList"):
            repo, kind, state_name, terms = parse_search(variables.get("q", ""))
            offset = int(variables.get("after") or 0)
            first = int(variables.get("first") or 30)
            failures = self.state.get("fail_list_limits", {})
            limit = str(offset + first)
            if failures.get(limit, 0):
                failures[limit] -= 1
                fail_graphql("Fixture list read refused")
            rows = self.search(kind, state_name, terms, repo is not None)
            page = rows[offset:offset + first]
            more = offset + first < len(rows)
            for entry in page:
                remember_kind(self.state, self.host, repo or self.repo(), entry["number"], kind)
            nodes = [gql_item(entry, kind, self.host, repo or self.repo(), False) for entry in page]
            data = {"search": {"issueCount": len(rows), "nodes": nodes,
                               "pageInfo": {"hasNextPage": more,
                                            "endCursor": str(offset + first) if more else None}}}
            if variables.get("other"):
                # The list header's other state, counted in the same request.
                other_repo, other_kind, other_state, other_terms = parse_search(variables["other"])
                data["other"] = {"issueCount": len(self.search(other_kind, other_state, other_terms,
                                                               other_repo is not None))}
            return {"data": data}
        if operation in ("IssueDetail", "PullRequestDetail", "PullRequestFingerprint"):
            kind = "issue" if operation == "IssueDetail" else "pr"
            number = int(variables["number"])
            if operation != "PullRequestFingerprint":
                remember_kind(self.state, self.host, self.repo(), number, kind)
                if self.state.get("fail_item_views", 0):
                    self.state["fail_item_views"] -= 1
                    fail_graphql("Fixture refresh refused")
            if self.state.get("strict_numbers") and not any(entry["number"] == number
                                                            for entry in self.entries(kind)):
                # GitHub's answer for a number that is not this kind of item (deep links probe).
                noun = "Issue" if kind == "issue" else "PullRequest"
                fail_graphql(f"Could not resolve to a {noun} with the number of {number}.", "NOT_FOUND")
            node = gql_item(self.find(kind, number), kind, self.host, self.repo(), True)
            if operation == "PullRequestFingerprint":
                node["commits"] = {"nodes": [{"commit": {"statusCheckRollup": (node["latest"]["nodes"][0]["commit"]
                                                                              ["statusCheckRollup"])}}]}
            field = "issue" if kind == "issue" else "pullRequest"
            return {"data": {"repository": {field: node}}}
        if operation == "ReviewThreads":
            return self.threads()
        if operation == "PullRequestFiles":
            files = self.find("pr", int(variables["number"])).get("files", [])
            viewed = self.state.get("viewed", [])
            return {"data": {"repository": {"pullRequest": {"files": {
                "pageInfo": {"hasNextPage": False, "endCursor": None},
                "nodes": [{"path": file["path"], "additions": file.get("additions", 0),
                           "deletions": file.get("deletions", 0), "changeType": "MODIFIED",
                           "viewerViewedState": "VIEWED" if file["path"] in viewed else "UNVIEWED"}
                          for file in files]}}}}}
        if operation == "AddThreadReply":
            if self.state.get("fail_reply", 0):
                self.state["fail_reply"] -= 1
                fail_graphql("GitHub rejected the thread reply")
            thread = variables["thread"]
            comment = {"author": {"login": "rowan"}, "body": variables["body"], "path": "example.rs", "line": 1,
                       "originalLine": 1}
            self.state.setdefault("thread_replies", {}).setdefault(thread, []).append(comment)
            source = next((key for key, threads in self.state.get("review_threads", {}).items()
                           if any(entry.get("id") == thread for entry in threads)), None)
            repository = source.split("#")[0] if source else self.repo()
            self.state.setdefault("reply_targets", []).append(
                {"host": self.host, "repository": repository, "thread": thread, "body": variables["body"]})
            return {"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "REPLY_NODE_1", "url": ""}}}}
        if operation in ("MarkFileViewed", "UnmarkFileViewed"):
            viewed = set(self.state.get("viewed", []))
            if operation == "UnmarkFileViewed":
                viewed.discard(variables["path"])
            else:
                viewed.add(variables["path"])
            self.state["viewed"] = sorted(viewed)
            return {"data": {"saved": {"pullRequest": {"id": variables.get("pullRequest")}}}}
        if operation in ("ResolveThread", "UnresolveThread"):
            resolved = operation == "ResolveThread"
            self.state.setdefault("thread_resolved", {})[variables["thread"]] = resolved
            return {"data": {"saved": {"thread": {"id": variables["thread"], "isResolved": resolved}}}}
        if operation == "UpdateSubscription":
            self.state.setdefault("subscriptions", []).append(variables)
            return {"data": {"updateSubscription": {"subscribable": {"id": variables.get("subject")}}}}
        fail_graphql("Unexpected fixture GraphQL operation: " + repr(operation))

    def threads(self):
        variables = self.request.variables
        source = f"{variables['owner']}/{variables['name']}#{variables['number']}"
        comments = [{"author": {"login": "reviewer"}, "body": "Please keep this draft.", "path": "example.rs",
                     "line": 1, "originalLine": 1, "commit": {"oid": "a" * 40}}]
        threads = json.loads(json.dumps(self.state.get("review_threads", {}).get(source, [
            {"id": "THREAD_1", "isResolved": self.state.get("resolved", False), "comments": {"nodes": comments}}])))
        resolved = self.state.get("thread_resolved", {})
        for thread in threads:
            thread["comments"]["nodes"].extend(self.state.get("thread_replies", {}).get(thread["id"], []))
            thread["isResolved"] = resolved.get(thread["id"], thread.get("isResolved", False))
        threads.extend({"id": f"INLINE_{index}", "isResolved": resolved.get(f"INLINE_{index}", False),
                        "comments": {"nodes": [comment]}}
                       for index, comment in enumerate(self.state.get("inline_comments", [])))
        nodes = []
        for thread in threads:
            first = (thread["comments"]["nodes"] or [{}])[0]
            nodes.append({"id": thread["id"], "isResolved": thread["isResolved"], "path": first.get("path"),
                          "line": first.get("line"), "originalLine": first.get("originalLine"),
                          "diffSide": first.get("diffSide", "RIGHT"),
                          "comments": {"totalCount": len(thread["comments"]["nodes"]), "nodes": [
                              dict({"id": f"{thread['id']}_COMMENT_{index}", "createdAt": "2026-11-08T10:00:00Z",
                                    "commit": {"oid": "a" * 40}}, **comment)
                              for index, comment in enumerate(thread["comments"]["nodes"])]}})
            # A multi-line thread's first line (GitHub's `startLine`).
            if thread.get("startLine"):
                nodes[-1]["startLine"] = thread["startLine"]
        # Two pages exercise the provider's connection paging, including an empty last page.
        second = variables.get("after") == "next"
        page = nodes[1:] if second else nodes[:1]
        return {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "nodes": page, "pageInfo": {"hasNextPage": not second, "endCursor": None if second else "next"}}}}}}

    # ---- REST

    def rest(self):
        request, state = self.request, self.state
        method, tail = request.method, request.tail
        if request.rest == "user":
            return 200, {"login": "rowan", "id": 1}
        if request.rest.startswith("notifications"):
            # A world may hold an inbox (`notifications`, GitHub thread objects); none is empty.
            return (200, state.get("notifications", [])) if method == "GET" else (205, None)
        if getattr(request, "counted", None) and request.rest == "search/issues":
            return self.anonymous_search()
        if tail is None:
            fail_rest(404, "Not Found")
        repo = request.repo
        if getattr(request, "counted", None) and method == "GET":
            found = self.anonymous_read(repo, tail)
            if found is not None:
                return found
        if tail == "" and method == "GET":
            info = state.get("repo_info", {}).get(repo)
            if info is None:
                name = state.get("repository", repo)
                info = {"full_name": name, "default_branch": "main"}
            host = state.get("host", self.host)
            info = dict({"node_id": "REPO_NODE", "html_url": f"https://{host}/{info['full_name']}",
                         "fork": False, "permissions": {"admin": False, "push": True, "pull": True}}, **info)
            return 200, info
        if method == "GET":
            return self.rest_read(repo, tail)
        return self.rest_write(repo, tail)

    # ---- F7: the REST reads an anonymous client makes instead of GraphQL

    def rest_item(self, entry, kind, repo, listed=False):
        host = self.state.get("host", self.host)
        word = str(entry.get("state", "OPEN")).upper()
        number = entry["number"]
        labels = [{"name": label.get("name"), "color": label.get("color", "d73a4a")} for label in entry.get("labels", [])]
        item = {"node_id": entry.get("id"), "number": number, "title": entry.get("title"), "body": entry.get("body"),
                "state": "open" if word == "OPEN" else "closed", "state_reason": entry.get("stateReason"),
                "html_url": f"https://{host}/{repo}/{'pull' if kind == 'pr' else 'issues'}/{number}",
                "created_at": entry.get("createdAt", "2026-11-08T10:00:00Z"),
                "updated_at": entry.get("updatedAt", "2026-11-08T10:00:00Z"),
                "user": {"login": (entry.get("author") or {}).get("login", "ghost")}, "labels": labels,
                "comments": len(entry.get("comments", [])) if isinstance(entry.get("comments"), list) else entry.get("comments", 0),
                "repository_url": f"https://api.{host}/repos/{repo}",
                "assignees": [{"login": a.get("login")} for a in entry.get("assignees", [])],
                "milestone": {"number": 1, "title": entry["milestone"]["title"], "state": "open"} if entry.get("milestone") else None}
        if kind == "pr":
            merged = "2026-11-09T10:00:00Z" if word == "MERGED" else None
            if listed:
                item["pull_request"] = {"merged_at": merged, "html_url": item["html_url"]}
            else:
                head = entry.get("headRefOid", "a" * 40)
                item.update({"merged_at": merged, "draft": entry.get("isDraft", False),
                             "head": {"ref": entry.get("headRefName", "feature"), "sha": head, "repo": {"full_name": repo}},
                             "base": {"ref": entry.get("baseRefName", "main"), "sha": "b" * 40, "repo": {"full_name": repo}},
                             "mergeable": entry.get("mergeable", "MERGEABLE") == "MERGEABLE",
                             "mergeable_state": str(entry.get("mergeStateStatus", "CLEAN")).lower(),
                             "additions": sum(f.get("additions", 0) for f in entry.get("files", [])),
                             "deletions": sum(f.get("deletions", 0) for f in entry.get("files", [])),
                             "changed_files": entry.get("changedFiles", len(entry.get("files", []))),
                             "requested_reviewers": [{"login": login} for login in entry.get("reviewRequests", [])],
                             "requested_teams": []})
        return item

    def anonymous_list(self, repo, kinds):
        state = self.request.query.get("state", "open")
        rows = []
        for kind in kinds:
            for entry in self.entries("issue" if kind == "issue" else "pr"):
                word = str(entry.get("state", "OPEN")).upper()
                if state == "all" or (state == "open") == (word == "OPEN"):
                    rows.append(self.rest_item(entry, kind, repo, listed=len(kinds) > 1))
        rows.sort(key=lambda row: row["created_at"], reverse=True)
        return rows

    def anonymous_search(self):
        repo, kind, state_name, terms = parse_search(self.request.query.get("q", ""))
        repo = repo or self.state.get("repository", "example/demo")
        rows = [self.rest_item(entry, kind, repo, listed=True)
                for entry in self.search("issue" if kind == "issue" else "pr", state_name, terms, True)]
        return 200, {"total_count": len(rows), "incomplete_results": False, "items": rows}

    def anonymous_read(self, repo, tail):
        """REST answers built from the same entries the GraphQL world serves; None = not one of them."""
        if tail == "pulls":
            return 200, self.anonymous_list(repo, ["pr"])
        if tail == "issues":
            return 200, self.anonymous_list(repo, ["issue", "pr"])
        match = re.fullmatch(r"issues/(\d+)", tail)
        if match:
            number = int(match.group(1))
            if any(entry["number"] == number for entry in self.entries("pr")):
                return 200, self.rest_item(self.find("pr", number), "pr", repo, listed=True)
            if not any(entry["number"] == number for entry in self.entries("issue")):
                fail_rest(404, "Not Found")
            return 200, self.rest_item(self.find("issue", number), "issue", repo)
        match = re.fullmatch(r"pulls/(\d+)(?:/(files|reviews|commits|comments))?", tail)
        if match and not self.request.wants_diff():
            number = int(match.group(1))
            if not any(entry["number"] == number for entry in self.entries("pr")):
                fail_rest(404, "Not Found")
            pr = self.find("pr", number)
            part = match.group(2)
            if part is None:
                return 200, self.rest_item(pr, "pr", repo)
            if part == "files":
                return 200, [{"filename": f["path"], "additions": f.get("additions", 0),
                              "deletions": f.get("deletions", 0), "status": "modified"} for f in pr.get("files", [])]
            if part == "reviews":
                return 200, [{"node_id": f"REVIEW_{i}", "user": r.get("author"), "state": r.get("state"),
                              "body": r.get("body", ""), "submitted_at": "2026-11-08T11:00:00Z"}
                             for i, r in enumerate(pr.get("reviews", []))]
            if part == "commits":
                return 200, [{"sha": c["oid"], "author": {"login": "alice"},
                              "commit": {"message": c.get("messageHeadline", "") + ("\n\n" + c["messageBody"] if c.get("messageBody") else ""),
                                         "committer": {"date": "2026-11-08T10:00:00Z"}}} for c in pr.get("commits", [])]
            return 200, self.state.get("review_comments", {}).get(str(number), [])
        match = re.fullmatch(r"commits/([0-9a-f]+)/(check-runs|status)", tail)
        if match:
            pr = next((entry for entry in self.entries("pr") if entry.get("headRefOid", "a" * 40) == match.group(1)), self.pr)
            checks = [check for check in pr.get("statusCheckRollup", []) if not is_context(check)]
            contexts = [check for check in pr.get("statusCheckRollup", []) if is_context(check)]
            if match.group(2) == "check-runs":
                return 200, {"total_count": len(checks), "check_runs": [
                    {"name": c.get("name"), "status": str(c.get("status", "COMPLETED")).lower(),
                     "conclusion": str(c.get("conclusion") or "").lower() or None, "details_url": c.get("detailsUrl"),
                     "started_at": "2026-11-08T10:00:00Z", "completed_at": "2026-11-08T10:01:00Z"} for c in checks]}
            return 200, {"state": "success", "statuses": [
                {"context": c.get("context"), "state": str(c.get("state", "SUCCESS")).lower(),
                 "target_url": c.get("targetUrl"), "created_at": "2026-11-08T10:00:00Z"} for c in contexts]}
        return None

    def rest_read(self, repo, tail):
        state = self.state
        if tail == "labels":
            return 200, [{"name": "bug", "color": "d73a4a"}, {"name": "native", "color": "0e8a16"}]
        if tail == "assignees":
            return 200, [{"login": "bob"}, {"login": "rowan"}]
        if tail == "milestones":
            return 200, state.get("milestones", [{"number": 1, "title": "v1", "state": "open"}])
        match = re.fullmatch(r"issues/(\d+)/timeline", tail)
        if match and match.group(1) in state.get("timelines", {}):
            # A scenario's own timeline for this item (screenshots); comments posted since follow.
            events = list(state["timelines"][match.group(1)])
            events.extend({"event": "commented", "user": {"login": "maya"}, "body": body,
                           "created_at": state.get("posted_at", "2026-11-08T10:00:00Z"),
                           "node_id": f"POSTED_{index}"} for index, body in enumerate(state.get("comments", [])))
            return 200, events
        if match:
            events = [{"event": "labeled", "actor": {"login": "alice"}, "label": {"name": "bug"},
                       "created_at": "2026-11-08"}]
            events.extend({"event": "commented", "user": {"login": "rowan"}, "body": body, "created_at": "2026-11-08",
                           "node_id": f"COMMENT_{index}"} for index, body in enumerate(state.get("comments", [])))
            return 200, events
        match = re.fullmatch(r"pulls/(\d+)", tail)
        if match and self.request.wants_diff():
            number = match.group(1)
            return 200, state.get("diffs", {}).get(number, state.get("diff", DEFAULT_DIFF))
        if match:
            # The head a suggestion is committed to.
            return 200, self.rest_item(self.find("pr", int(match.group(1))), "pr", repo)
        if tail == "pulls":
            return 200, []
        match = re.fullmatch(r"contents/(.+)", tail)
        if match and match.group(1) in state.get("contents", {}):
            # A file at the head branch (`state["contents"]`: path → text), as the contents API
            # sends it: base64 in 60-column lines and the blob's sha.
            text = state["contents"][match.group(1)]
            encoded = base64.b64encode(text.encode()).decode()
            return 200, {"type": "file", "path": match.group(1), "sha": blob_sha(text), "encoding": "base64",
                         "content": "\n".join(encoded[at:at + 60] for at in range(0, len(encoded), 60)) + "\n"}
        match = re.fullmatch(r"commits/([0-9a-f]+)", tail)
        if match:
            if state.get("fail_commit_views", 0):
                state["fail_commit_views"] -= 1
                fail_rest(502, "Fixture commit refresh refused")
            return 200, state.get("commit_diffs", {}).get(match.group(1), COMMIT_DIFF)
        if tail == "actions/workflows":
            return 200, {"total_count": len(self.workflows), "workflows": self.workflows}
        match = re.fullmatch(r"actions/workflows/(\d+)", tail)
        if match:
            workflow = next((flow for flow in self.workflows if flow["id"] == int(match.group(1))), None)
            if workflow is None:
                fail_rest(404, "Not Found")
            return 200, workflow
        if tail.startswith("contents/.github/workflows/"):
            return 200, WORKFLOW_YAML
        match = re.fullmatch(r"actions/(?:workflows/(\d+)/)?runs", tail)
        if match:
            self.refresh_run(False)
            runs = self.all_runs()
            if match.group(1) and self.runs:
                runs = [entry for entry in runs if entry.get("workflowDatabaseId") == int(match.group(1))]
            return 200, {"total_count": len(runs), "workflow_runs": [rest_run(entry, self.host, repo) for entry in runs]}
        match = re.fullmatch(r"actions/runs/(\d+)(?:/attempts/(\d+))?(/jobs)?", tail)
        if match:
            self.refresh_run(match.group(3) is None)
            run = self.run_by_id(int(match.group(1)))
            if match.group(2):
                run["attempt"] = int(match.group(2))
            if match.group(3):
                jobs = [rest_job(job, run, self.host, repo) for job in run.get("jobs", [])]
                return 200, {"total_count": len(jobs), "jobs": jobs}
            return 200, rest_run(run, self.host, repo)
        match = re.fullmatch(r"actions/jobs/(\d+)/logs", tail)
        if match:
            return 200, strip_gh_columns(state.get("logs", {}).get(match.group(1), state.get("log", DEFAULT_LOG)))
        match = re.fullmatch(r"actions/jobs/(\d+)", tail)
        if match:
            self.refresh_run(False)
            found = next(((job, entry) for entry in self.all_runs() for job in entry.get("jobs", [])
                          if job["databaseId"] == int(match.group(1))), None)
            if found is None:
                fail_rest(404, "Not Found")
            return 200, rest_job(found[0], found[1], self.host, repo)
        if re.fullmatch(r"actions/runs/\d+/artifacts", tail):
            return 200, {"total_count": 0, "artifacts": []}
        fail_rest(404, "Unexpected fixture read: " + tail)

    def rest_write(self, repo, tail):
        state, body = self.state, self.request.json or {}
        method = self.request.method
        match = re.fullmatch(r"issues/(\d+)/comments", tail)
        if match and method == "POST":
            if state.get("fail_comments", 0):
                state["fail_comments"] -= 1
                fail_rest(422, "GitHub rejected the comment")
            state.setdefault("comments", []).append(body.get("body", ""))
            return 201, {"id": 1, "node_id": "ISSUE_COMMENT_1", "body": body.get("body", ""),
                         "html_url": f"https://{self.host}/{repo}/issues/{match.group(1)}#issuecomment-1"}
        match = re.fullmatch(r"(issues|pulls)/(\d+)", tail)
        if match and method == "PATCH":
            if "state" in body and match.group(1) == "issues":
                state["issue_state"] = "CLOSED" if body["state"] == "closed" else "OPEN"
            return 200, {"number": int(match.group(2)), "html_url": f"https://{self.host}/{repo}/issues/{match.group(2)}"}
        if tail == "issues" and method == "POST":
            return 201, {"number": 10, "html_url": f"https://{self.host}/{repo}/issues/10"}
        if tail == "pulls" and method == "POST":
            if state.get("fail_create"):
                state["fail_create"] -= 1
                fail_rest(422, "GitHub rejected PR creation")
            return 201, {"number": 9, "html_url": f"https://github.com/{repo}/pull/9"}
        if re.fullmatch(r"issues/\d+/(labels(/.*)?|assignees)|pulls/\d+/requested_reviewers", tail):
            return 200, {}
        match = re.fullmatch(r"pulls/(\d+)/comments", tail)
        if match and method == "POST":
            state.setdefault("inline_comments", []).append(
                {"author": {"login": "rowan"}, "body": body.get("body", ""), "path": body.get("path", ""),
                 "line": int(body.get("line") or 0), "originalLine": int(body.get("line") or 0),
                 "diffSide": body.get("side", "RIGHT"), "commit": {"oid": body.get("commit_id", "")}})
            return 201, {"id": 1, "node_id": "COMMENT_NODE_1", "body": body.get("body", "")}
        if re.fullmatch(r"pulls/\d+/reviews", tail):
            return 200, {"id": 1, "state": body.get("event", "COMMENTED")}
        if re.fullmatch(r"pulls/\d+/merge", tail):
            return 200, {"merged": True, "sha": body.get("sha", ""), "message": "Pull Request successfully merged"}
        match = re.fullmatch(r"contents/(.+)", tail)
        if match and method == "PUT":
            # A committed suggestion: the file is replaced when the blob it was read at is current.
            files = state.setdefault("contents", {})
            path = match.group(1)
            if path not in files or body.get("sha") != blob_sha(files[path]):
                fail_rest(409, f"{path} does not match {body.get('sha')}")
            files[path] = base64.b64decode(body.get("content", "")).decode()
            return 200, {"content": {"path": path, "sha": blob_sha(files[path])},
                         "commit": {"sha": "c" * 40, "message": body.get("message", "")}}
        if re.fullmatch(r"actions/workflows/\d+/dispatches", tail):
            return 204, None
        if re.fullmatch(r"actions/runs/\d+/(rerun|rerun-failed-jobs|cancel|approve)|actions/jobs/\d+/rerun", tail):
            return 201, {}
        fail_rest(404, "Unexpected fixture write: " + method + " " + tail)

    def porcelain(self, argv):
        writes = (["issue", "close"], ["issue", "reopen"], ["issue", "edit"], ["issue", "create"],
                  ["issue", "develop"], ["pr", "create"], ["pr", "edit"], ["pr", "review"], ["pr", "merge"],
                  ["workflow", "run"], ["run", "cancel"], ["run", "rerun"])
        if argv[:2] in writes:
            return 0, "Saved fixture operation\n", ""
        if len(argv) >= 3 and argv[1] == "view" and "--web" in argv:
            self.state.setdefault("browsers", []).append(argv)
            return 0, "", ""
        return 1, "", "Unexpected fixture command: " + repr(argv) + "\n"


# --------------------------------------------------------------------------- the inbox world


class Inbox:
    """`tests/fixtures/github_inbox_cli.py`'s strict multi-repository world."""

    def __init__(self, state, request):
        self.state, self.request = state, request
        self.host = state.get("host", "github.com")

    def check_host(self):
        if self.request.host != self.host:
            raise Reply(400, {"message": "API request was not host scoped: " + self.request.host})

    def item(self, repository, kind, number=7):
        search_state = self.state.get("search_state")
        state = ({"merged": "MERGED", "closed": "CLOSED"}.get(search_state, "OPEN")
                 if kind == "pr" and repository != "local/root" else "OPEN")
        return {"number": number, "id": f"{repository}-{kind}-{number}", "title": f"{repository} item {number}",
                "body": f"Canonical source for {repository} {kind} {number}", "state": state,
                "author": {"login": "alice"}, "labels": [], "assignees": [], "createdAt": "2026-11-08T10:00:00Z",
                "updatedAt": "2026-11-09T10:00:00Z", "commentsCount": 2,
                "repository": {"nameWithOwner": repository},
                "url": f"https://{self.host}/{repository}/{'issues' if kind == 'issue' else 'pull'}/{number}",
                "headRefOid": "a" * 40, "headRefName": "topic", "baseRefName": "main", "isDraft": False,
                "commits": [], "files": [{"path": repository.split('/')[0] + ".rs", "additions": 1, "deletions": 1}],
                "reviews": [], "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": [],
                "closingIssuesReferences": [], "closedByPullRequestsReferences": []}

    def graphql(self):
        self.check_host()
        request, variables = self.request, self.request.variables
        operation = request.operation
        if operation in ("IssueList", "PullRequestList"):
            repo, kind, state_name, terms = parse_search(variables.get("q", ""))
            offset = int(variables.get("after") or 0)
            first = int(variables.get("first") or 30)
            if repo is not None:
                rows = [self.item(repo, kind)]
            else:
                limit = str(offset + first)
                if limit in self.state.get("fail_limits", []):
                    self.state["fail_limits"].remove(limit)
                    fail_graphql("Search index/rate-limit unavailable")
                count = self.state.get("count", 2)
                repos = ["alpha/one", "beta/two"]
                rows = [self.item(repos[index % 2], kind, 7 + index // 2) for index in range(count)]
                if "label:empty" in terms:
                    rows = []
                if self.state.get("reverse_search"):
                    rows.reverse()
                self.state["search_state"] = state_name
                for row in rows:
                    row["state"] = {"merged": "MERGED", "closed": "CLOSED"}.get(state_name, "OPEN")
            page = rows[offset:offset + first]
            more = offset + first < len(rows)
            for row in page:
                remember_kind(self.state, self.host, row["repository"]["nameWithOwner"], row["number"], kind)
            return {"data": {"search": {"issueCount": len(rows), "pageInfo": {
                "hasNextPage": more, "endCursor": str(offset + first) if more else None},
                "nodes": [gql_item(row, kind, self.host, row["repository"]["nameWithOwner"], False) for row in page]}}}
        repo = request.repo
        if operation in ("IssueDetail", "PullRequestDetail", "PullRequestFingerprint"):
            kind = "issue" if operation == "IssueDetail" else "pr"
            number = int(variables["number"])
            remember_kind(self.state, self.host, repo, number, kind)
            node = gql_item(self.item(repo, kind, number), kind, self.host, repo, True)
            return {"data": {"repository": {"issue" if kind == "issue" else "pullRequest": node}}}
        if operation == "ReviewThreads":
            return {"data": {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": [], "pageInfo": {"hasNextPage": False, "endCursor": None}}}}}}
        if operation == "PullRequestFiles":
            return {"data": {"repository": {"pullRequest": {"files": {
                "pageInfo": {"hasNextPage": False, "endCursor": None},
                "nodes": [{"path": repo.split("/")[0] + ".rs", "additions": 1, "deletions": 1,
                           "changeType": "MODIFIED", "viewerViewedState": "UNVIEWED"}]}}}}}
        fail_graphql("Unexpected inbox GraphQL operation: " + repr(operation))

    def rest(self):
        self.check_host()
        request, state = self.request, self.state
        if request.rest == "user":
            return 200, {"login": "rowan", "id": 1}
        repo, tail = request.repo, request.tail
        if tail is None:
            fail_rest(404, "Not Found")
        if tail == "" and request.method == "GET":
            return 200, {"full_name": repo, "default_branch": "main", "html_url": f"https://{self.host}/{repo}"}
        if re.fullmatch(r"issues/\d+/timeline", tail):
            return 200, []
        if tail in ("labels", "assignees", "milestones"):
            return 200, []
        match = re.fullmatch(r"pulls/(\d+)", tail)
        if match and request.wants_diff():
            filename = repo.split("/")[0] + ".rs"
            return 200, (f"diff --git a/{filename} b/{filename}\n--- a/{filename}\n+++ b/{filename}\n@@ -1 +1 @@\n"
                         f"-old\n+source_for_{repo.split('/')[0]}\n")
        match = re.fullmatch(r"issues/(\d+)/comments", tail)
        if match and request.method == "POST":
            number = int(match.group(1))
            body = (request.json or {}).get("body", "")
            attempt = {"host": self.host, "repo": repo, "number": number, "body": body,
                       "kind": kind_of(state, self.host, repo, number)}
            state.setdefault("write_attempts", []).append(attempt)
            if repo in state.get("fail_comment_repos", []):
                state["fail_comment_repos"].remove(repo)
                fail_rest(422, "Fixture comment write refused")
            state.setdefault("writes", []).append(attempt)
            return 201, {"id": 1, "html_url": f"https://{self.host}/{repo}/comment/1", "body": body}
        fail_rest(404, "Unexpected fake inbox request: " + request.method + " " + tail)

    def porcelain(self, argv):
        target = flag(argv, "--repo")
        if argv[:1] and argv[0] in ("issue", "pr") and not target:
            return 1, "", "Repository command omitted its host-qualified --repo\n"
        repo = "local/root"
        if target:
            parts = target.split("/")
            if len(parts) != 3 or parts[0] != self.host or not all(parts):
                return 1, "", "CLI repository host mismatch: " + target + "\n"
            repo = "/".join(parts[1:])
        if len(argv) >= 3 and argv[0] in ("issue", "pr") and argv[1] == "view" and "--web" in argv:
            self.state.setdefault("browsers", []).append({"host": self.host, "repo": repo, "number": int(argv[2])})
            return 0, "", ""
        if len(argv) >= 2 and argv[1] in ("checkout", "create", "develop"):
            return 1, "", "Local Git mutation was attempted by an inbox test\n"
        return 1, "", "Unexpected fake-gh request: " + repr(argv) + "\n"


def flag(argv, name, default=""):
    return argv[argv.index(name) + 1] if name in argv and argv.index(name) + 1 < len(argv) else default


# --------------------------------------------------------------------------- call descriptors


def write_fields(body):
    """`gh api -f k=v / -F k=json` for a JSON body, the legacy fixture's raw-API write form."""
    fields = []
    for key, value in (body or {}).items():
        if isinstance(value, str):
            fields += ["-f", f"{key}={value}"]
        elif isinstance(value, list):
            for entry in value:
                fields += ["-f", f"{key}[]={entry}"]
        else:
            fields += ["-F", f"{key}={json.dumps(value)}"]
    return fields


def describe(state, request, world):
    """The gh argv the legacy fixtures saw for this request, or None for a secondary request."""
    host = request.host
    if request.graphql:
        variables = request.variables
        operation = request.operation
        if operation in ("IssueList", "PullRequestList"):
            repo, kind, state_name, terms = parse_search(variables.get("q", ""))
            offset = int(variables.get("after") or 0)
            limit = str(offset + int(variables.get("first") or 30))
            after = ["--after", variables["after"]] if variables.get("after") else []
            if repo is None:
                fields = [field for field in SEARCH_FIELDS + (["isDraft"] if kind == "pr" else [])
                          if field in request.document or field in ("commentsCount", "url")]
                scope = ["--merged"] if state_name == "merged" else ["--state", state_name]
                return ["search", "prs" if kind == "pr" else "issues", "--limit", limit, "--json",
                        ",".join(fields)] + scope + after + ["--", terms]
            fields = [field for field in (PR_LIST_FIELDS if kind == "pr" else ISSUE_LIST_FIELDS)
                      if field in request.document]
            return [kind, "list", "--state", state_name, "--search", terms, "--limit", limit, "--json",
                    ",".join(fields)] + after + ["--repo", f"{host}/{repo}"]
        if operation in ("IssueDetail", "PullRequestDetail"):
            kind = "issue" if operation == "IssueDetail" else "pr"
            fields = ISSUE_VIEW_FIELDS if kind == "issue" else PR_VIEW_FIELDS
            return [kind, "view", str(variables["number"]), "--json", ",".join(fields), "--repo",
                    f"{host}/{request.repo}"]
        if operation == "PullRequestFingerprint":
            return None
        argv = ["api", "graphql", "--hostname", host]
        if request.mutation:
            argv += ["--method", "POST"]
        # gh's `-f query=mutation(...)` form: the legacy assertions look for `mutation(`.
        argv.append("query=" + re.sub(r"^\s*(query|mutation)\s+\w+\s*\(", r"\1(", request.document))
        argv += [f"{key}={value}" for key, value in variables.items() if value is not None]
        return argv
    rest, tail, repo = request.rest, request.tail, request.repo
    if rest == "user":
        return None
    method = request.method
    scope = ["--repo", f"{host}/{repo}"]
    if method == "GET":
        if tail == "":
            return ["repo", "view", "--repo", f"{host}/{repo}"]
        match = re.fullmatch(r"pulls/(\d+)", tail or "")
        if match and request.wants_diff():
            return ["pr", "diff", match.group(1)] + scope
        match = re.fullmatch(r"actions/workflows/(\d+)", tail or "")
        if match:
            return None
        match = re.fullmatch(r"contents/(.+)", tail or "")
        if match:
            workflow = next((flow for flow in WORKFLOWS if flow["path"] == match.group(1)), None)
            ref = ["--ref", request.query["ref"]] if request.query.get("ref") else []
            return ["workflow", "view", str(workflow["id"] if workflow else match.group(1)), "--yaml"] + ref + scope
        match = re.fullmatch(r"actions/(?:workflows/(\d+)/)?runs", tail or "")
        if match:
            workflow = ["--workflow", match.group(1)] if match.group(1) else []
            return ["run", "list", "--limit", request.query.get("per_page", "20")] + workflow + scope
        match = re.fullmatch(r"actions/runs/(\d+)(?:/attempts/(\d+))?(/jobs)?", tail or "")
        if match:
            if match.group(3):
                return None
            attempt = ["--attempt", match.group(2)] if match.group(2) else []
            return ["run", "view", match.group(1)] + attempt + scope
        match = re.fullmatch(r"actions/jobs/(\d+)/logs", tail or "")
        if match:
            return ["run", "view", "--job", match.group(1), "--log"] + scope
        return ["api", "--hostname", host, request.path_and_query()]
    body = request.json or {}
    match = re.fullmatch(r"issues/(\d+)/comments", tail or "")
    if match and method == "POST":
        number = int(match.group(1))
        return [kind_of(state, host, repo, number), "comment", str(number), "--body", body.get("body", "")] + scope
    match = re.fullmatch(r"(issues|pulls)/(\d+)", tail or "")
    if match and method == "PATCH":
        number = int(match.group(2))
        kind = "pr" if match.group(1) == "pulls" else kind_of(state, host, repo, number)
        if "state" in body:
            return [kind, "close" if body["state"] == "closed" else "reopen", str(number)] + scope
        if "milestone" in body:
            if body["milestone"] is None:
                return [kind, "edit", str(number), "--remove-milestone"] + scope
            titles = {entry["number"]: entry["title"] for entry in state.get("milestones", [{"number": 1, "title": "v1"}])}
            return [kind, "edit", str(number), "--milestone", titles.get(body["milestone"], str(body["milestone"]))] + scope
        return [kind, "edit", str(number), "--title", body.get("title", ""), "--body", body.get("body", "")] + scope
    match = re.fullmatch(r"issues/(\d+)/labels(?:/(.+))?", tail or "")
    if match:
        number = int(match.group(1))
        kind = kind_of(state, host, repo, number)
        if method == "DELETE":
            return [kind, "edit", str(number), "--remove-label", urllib.parse.unquote(match.group(2) or "")] + scope
        return [kind, "edit", str(number), "--add-label", ",".join(body.get("labels", []))] + scope
    match = re.fullmatch(r"issues/(\d+)/assignees", tail or "")
    if match:
        number = int(match.group(1))
        word = "--remove-assignee" if method == "DELETE" else "--add-assignee"
        return [kind_of(state, host, repo, number), "edit", str(number), word,
                ",".join(body.get("assignees", []))] + scope
    match = re.fullmatch(r"pulls/(\d+)/requested_reviewers", tail or "")
    if match:
        word = "--remove-reviewer" if method == "DELETE" else "--add-reviewer"
        names = body.get("reviewers", []) + body.get("team_reviewers", [])
        return ["pr", "edit", match.group(1), word, ",".join(names)] + scope
    if tail == "issues" and method == "POST":
        argv = ["issue", "create", "--title", body.get("title", ""), "--body", body.get("body", "")]
        for label in body.get("labels", []):
            argv += ["--label", label]
        for assignee in body.get("assignees", []):
            argv += ["--assignee", assignee]
        return argv + scope
    match = re.fullmatch(r"pulls/(\d+)/reviews", tail or "")
    if match:
        verdict = {"APPROVE": "--approve", "REQUEST_CHANGES": "--request-changes"}.get(body.get("event"), "--comment")
        return ["pr", "review", match.group(1), verdict, "--body", body.get("body", "")] + scope
    match = re.fullmatch(r"pulls/(\d+)/merge", tail or "")
    if match:
        return ["pr", "merge", match.group(1), "--match-head-commit", body.get("sha", ""),
                "--" + body.get("merge_method", "merge")] + scope
    match = re.fullmatch(r"actions/workflows/(\d+)/dispatches", tail or "")
    if match:
        argv = ["workflow", "run", match.group(1), "--ref", body.get("ref", "")]
        for key, value in (body.get("inputs") or {}).items():
            argv += ["--raw-field", f"{key}={value}"]
        return argv + scope
    match = re.fullmatch(r"actions/runs/(\d+)/cancel", tail or "")
    if match:
        return ["run", "cancel", match.group(1)] + scope
    match = re.fullmatch(r"actions/runs/(\d+)/(rerun|rerun-failed-jobs)", tail or "")
    if match:
        return ["run", "rerun", match.group(1)] + (["--failed"] if match.group(2) != "rerun" else []) + scope
    return ["api", "--hostname", host, request.path_and_query(), "--method", method] + write_fields(body)


# --------------------------------------------------------------------------- recorded layer


def load_recorded(directory):
    root = pathlib.Path(directory)
    data = json.loads((root / "requests.json").read_text())
    for entry in data["entries"]:
        RECORDED.setdefault(entry["key"], []).append((entry, root / "bodies" / entry["body"] if entry.get("body") else None))


def recorded_key(request):
    if request.graphql:
        return f"POST /graphql op={request.operation}"
    pairs = sorted((key, value) for key, value in request.query.items() if key not in ("per_page", "limit"))
    query = urllib.parse.urlencode(pairs)
    return f"{request.method} /{request.rest}" + ("?" + query if query else "")


def recorded(request):
    entries = RECORDED.get(recorded_key(request))
    if not entries:
        return None
    wanted = request.accept
    chosen = next((pair for pair in entries if pair[0].get("accept") and pair[0]["accept"] in wanted), None)
    chosen = chosen or next((pair for pair in entries if not pair[0].get("accept")), entries[0])
    entry, path = chosen
    body = path.read_bytes() if path else b""
    headers = {key: value for key, value in entry.get("headers", {}).items() if key not in ("link",)}
    return entry["status"], body, headers


# --------------------------------------------------------------------------- dispatch


def gate_for(state, entry, repo):
    call = entry.get("call") or []
    for gate in state.get("read_gates", []) + state.get("gates", []):
        if "repo" in gate and gate["repo"] != repo:
            continue
        if "prefix" in gate and call[:len(gate["prefix"])] == gate["prefix"]:
            return gate
        if "contains" in gate and any(gate["contains"] in arg for arg in call):
            return gate
        # Any world: the request path ends with this (`/actions/runs`, `/pipelines`).
        if "path" in gate and str(entry.get("path") or "").endswith(gate["path"]):
            return gate
    return None


def wait_gate(gate):
    release = pathlib.Path(ARGS.state + "." + gate["name"])
    deadline = time.monotonic() + 20
    while not release.exists():
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)
    return True


def matching_fault(state, request):
    for rule in state.get("fail", []):
        if rule.get("times", 1) <= 0:
            continue
        if rule.get("transport") and rule["transport"] != request.transport:
            continue
        if rule.get("method") and rule["method"] != request.method:
            continue
        if rule.get("operation") and rule["operation"] != request.operation:
            continue
        if rule.get("path") and rule["path"] not in (request.rest or ""):
            continue
        # `query`: a substring of the decoded query string (`created_by=nobody`).
        if rule.get("query") and rule["query"] not in urllib.parse.unquote(request.query_string or ""):
            continue
        rule["times"] = rule.get("times", 1) - 1
        return rule
    return None


def world_for(state, request):
    world = state.get("world") or ARGS.world
    return Inbox(state, request) if world == "inbox" else Legacy(state, request)


# F7: anonymous reads. With `state["anonymous"] = {limit, remaining, reset, style,
# sign_in_required, refuse}`, a direct-HTTP request without a credential is answered like a public
# forge answers it: writes, GitHub GraphQL, GitLab mutations and the reads each forge keeps behind a
# sign-in are 401 (403 on GitHub), a Gitea/Forgejo instance with `sign_in_required` refuses every
# API call with 403, and the rest is answered by the world as if signed in. Every counted answer
# carries rate headers (`style` "github" = `x-ratelimit-*`, "ietf" = `ratelimit` /
# `ratelimit-policy`); `remaining` falls by one per answer except a 304, and at 0 GitHub answers
# 403 "API rate limit exceeded". `state["anonymous_refused"]` counts the refusals.
ANONYMOUS_SIGNED_IN = {
    "github": [r"user", r"notifications.*", r"repos/[^/]+/[^/]+/actions/jobs/\d+/logs",
               r"repos/[^/]+/[^/]+/actions/artifacts/\d+/zip"],
    "gitlab": [r"user", r"todos.*",
               r"projects/[^/]+/(labels|milestones|members/all)",
               r"projects/[^/]+/(merge_requests|issues)/\d+/(notes|discussions)",
               r"projects/[^/]+/jobs/\d+(/trace)?"],
    "gitea": [r"user", r"notifications.*", r"repos/[^/]+/[^/]+/(collaborators.*|assignees)"],
}


def anonymous_gate(state, request):
    """Raise the refusal a public forge sends an anonymous `request`, or mark it as answerable."""
    anonymous = state["anonymous"]

    def refuse(status, body):
        state["anonymous_refused"] = state.get("anonymous_refused", 0) + 1
        raise Reply(status, body)

    if request.api == "gitea" and anonymous.get("sign_in_required"):
        refuse(403, {"message": "Only signed in user is allowed to call APIs."})
    if request.graphql:
        if (state.get("world") or ARGS.world) != "gitlab":
            # GitHub's GraphQL API has no anonymous access.
            refuse(401, {"message": "This endpoint requires you to be authenticated.",
                         "documentation_url": "https://docs.github.com/graphql/guides/forming-calls-with-graphql"})
        if request.mutation:
            refuse(401, {"message": "401 Unauthorized"})
    elif request.method not in ("GET", "HEAD"):
        refuse(401, {"message": "Requires authentication" if request.api == "github" else "401 Unauthorized"})
    path = (request.rest or "").split("?", 1)[0]
    patterns = ANONYMOUS_SIGNED_IN.get(request.api, []) + list(anonymous.get("refuse", []))
    if not request.graphql and any(re.fullmatch(pattern, path) for pattern in patterns):
        if request.api == "github" and "/actions/" in path:
            refuse(403, {"message": "Must have admin rights to Repository.",
                         "documentation_url": "https://docs.github.com/rest/actions/workflow-jobs"})
        refuse(401, {"message": "Requires authentication" if request.api == "github" else "401 Unauthorized"})
    search = request.api == "github" and path.startswith("search/")
    if "limit" in anonymous and not search and anonymous.get("remaining", anonymous["limit"]) <= 0:
        state["anonymous_refused"] = state.get("anonymous_refused", 0) + 1
        request.counted = "core"
        raise Reply(403, {"message": "API rate limit exceeded for 127.0.0.1. (But here's the good news: "
                                     "Authenticated requests get a higher rate limit.)",
                          "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api"})
    request.counted = "search" if search else "core"
    request.auth = "valid"


def anonymous_headers(state, request, status):
    """Charge an answered anonymous request to the quota and return its rate headers."""
    anonymous = state.get("anonymous") or {}
    if "limit" not in anonymous or not getattr(request, "counted", None):
        return {}
    limit = int(anonymous["limit"])
    reset = int(anonymous.get("reset", 3600))
    if request.counted == "search":
        return {"x-ratelimit-limit": "10", "x-ratelimit-remaining": "9", "x-ratelimit-reset": str(int(time.time()) + 60),
                "x-ratelimit-resource": "search"}
    remaining = int(anonymous.get("remaining", limit))
    if status != 304 and remaining > 0:
        remaining -= 1
    anonymous["remaining"] = remaining
    anonymous["charged"] = anonymous.get("charged", 0) + (status != 304)
    if anonymous.get("style") == "ietf":
        return {"ratelimit": f'"default";r={remaining};t={reset}', "ratelimit-policy": f'"default";q={limit};w=3600'}
    return {"x-ratelimit-limit": str(limit), "x-ratelimit-remaining": str(remaining),
            "x-ratelimit-reset": str(int(time.time()) + reset), "x-ratelimit-used": str(limit - remaining),
            "x-ratelimit-resource": "core"}


PROBES = ("api/v1/version", "api/forgejo/v1/version", "api/v4/version", "api/v3/meta")


def answer(state, request):
    world = state.get("world") or ARGS.world
    if request.full in PROBES:
        probe = state.get("probes", {}).get(request.host, {}).get(request.full)
        state["probe_count"] = state.get("probe_count", 0) + 1
        if not probe:
            raise Reply(404, {"message": "Not Found"})
        raise Reply(probe.get("status", 200), probe.get("body", {}), probe.get("headers"))
    if request.transport == "http" and request.auth == "missing" and state.get("anonymous") is not None:
        anonymous_gate(state, request)
    if request.transport == "http":
        if request.auth != "valid" or state.get("reject_http_auth"):
            raise Reply(401, {"message": "Bad credentials", "documentation_url": "https://docs.github.com/rest"})
    fault = matching_fault(state, request)
    if fault:
        if "location" in fault:
            raise Reply(fault.get("status", 302), b"", {"Location": fault["location"]})
        raise Reply(fault.get("status", 500), fault.get("body", {"message": fault.get("message", "Fault")}),
                    fault.get("headers"))
    if RECORDED:
        hit = recorded(request)
        if hit:
            raise Reply(*hit)
    if world in ("gitea", "forgejo") and request.api == "gitea":
        try:
            return world_gitea.answer(state, request, world)
        except world_gitea.Reply as reply:
            raise Reply(reply.status, reply.body, reply.headers)
    if world == "actions" and request.api == "github":
        try:
            return world_actions.answer(state, request)
        except world_actions.Reply as reply:
            raise Reply(reply.status, reply.body, reply.headers)
    if world == "gitlab":
        try:
            return world_gitlab.answer(state, request)
        except world_gitlab.Reply as reply:
            raise Reply(reply.status, reply.body, reply.headers)
    if request.api != "github":
        if request.method == "GET" and request.rest == "user":
            viewer = state.get("viewer", "rowan")
            return 200, {"id": 1, "login": viewer, "username": viewer}
        hit = state.get("answers", {}).get(f"{request.method} {request.rest}")
        if not hit:
            raise Reply(404, {"message": "404 Not Found"})
        raise Reply(hit.get("status", 200), hit.get("body", {}), hit.get("headers"))
    if world == "recorded":
        fail_rest(404, "Not recorded: " + recorded_key(request))
    target = world_for(state, request)
    if request.graphql:
        return 200, target.graphql()
    return target.rest()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def respond(self, status, body, headers=None):
        headers = dict(headers or {})
        if isinstance(body, (dict, list)):
            payload = json.dumps(body).encode()
            headers.setdefault("Content-Type", "application/json; charset=utf-8")
        elif isinstance(body, str):
            payload = body.encode()
            headers.setdefault("Content-Type", "text/plain; charset=utf-8")
        elif body is None:
            payload = b""
        else:
            payload = body
        self.send_response(status)
        for key, value in headers.items():
            if key.lower() not in ("content-length", "transfer-encoding", "connection"):
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(payload)

    def handle_any(self):
        if self.path.startswith("/_fake/") or self.path.startswith("/_blob/"):
            return self.handle_api()
        with FLIGHT_LOCK:
            FLIGHT["now"] += 1
            FLIGHT["max"] = max(FLIGHT["max"], FLIGHT["now"])
        try:
            return self.handle_api()
        finally:
            with FLIGHT_LOCK:
                FLIGHT["now"] -= 1

    def handle_api(self):
        try:
            if self.path.startswith("/_fake/"):
                return self.fake()
            if self.path.startswith("/_blob/"):
                return self.blob()
            if self.offline():
                return
            request = Request(self)
            with LOCK:
                state = load_state()
                world = state.get("world") or ARGS.world
                if request.auth == "invalid" and request.token in state.get("tokens", []):
                    request.auth = "valid"
                entry = {"transport": request.transport, "method": request.method, "host": request.host,
                         "path": request.rest if not request.graphql else "graphql", "query": request.query,
                         "operation": request.operation, "variables": request.variables or None,
                         "body": request.json if not request.graphql else None, "auth": request.auth,
                         "gh_argv": request.gh_argv,
                         "call": describe(state, request, world)
                         if world not in ("recorded", "gitlab", "gitea", "forgejo", "actions") and request.api == "github" and request.full not in PROBES
                         else None}
                state.setdefault("requests", []).append(entry)
                gate = gate_for(state, entry, request.repo)
                if gate is None:
                    try:
                        status, body = answer(state, request)
                        headers = {}
                    except Reply as reply:
                        status, body, headers = reply.status, reply.body, reply.headers
                save_state(state)
            if gate is not None:
                released = wait_gate(gate)
                with LOCK:
                    state = load_state()
                    if not released:
                        status, body, headers = 502, {"message": "Fixture read gate timed out: " + gate["name"]}, {}
                    else:
                        try:
                            status, body = answer(state, request)
                            headers = {}
                        except Reply as reply:
                            status, body, headers = reply.status, reply.body, reply.headers
                    save_state(state)
            if request.method == "GET" and status == 200 and not request.graphql:
                # Conditional requests: a strong ETag over the body, 304 on match.
                if isinstance(body, (dict, list)):
                    encoded = json.dumps(body).encode()
                elif isinstance(body, str):
                    encoded = body.encode()
                else:
                    encoded = body or b""
                etag = '"' + hashlib.sha1(encoded).hexdigest() + '"'
                headers = dict(headers or {})
                headers.setdefault("ETag", etag)
                if self.headers.get("If-None-Match") == etag:
                    status, body = 304, None
                    with LOCK:
                        state = load_state()
                        state["not_modified"] = state.get("not_modified", 0) + 1
                        save_state(state)
            if getattr(request, "counted", None):
                with LOCK:
                    state = load_state()
                    headers = dict(headers or {})
                    headers.update(anonymous_headers(state, request, status))
                    save_state(state)
            delay = state.get("delay_ms", 0)
            # `slow_paths`: {"fragment": ms} slows only the requests whose path holds it.
            for fragment, millis in (state.get("slow_paths") or {}).items():
                if fragment in self.path:
                    delay = max(delay, millis)
            if delay:
                time.sleep(delay / 1000)
            self.respond(status, body, headers)
        except Exception as error:  # a fixture bug must show up in the test, not hang it
            self.respond(500, {"message": "fixture server error: " + repr(error)})

    def offline(self):
        """`state["offline"]`: the network is gone. Every API connection drops without an answer
        (a reset, as a dead link or a failed lookup reaches the client); `offline_refused` counts
        them."""
        with LOCK:
            state = load_state()
            if not state.get("offline"):
                return False
            state["offline_refused"] = state.get("offline_refused", 0) + 1
            save_state(state)
        self.close_connection = True
        try:
            self.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        return True

    def fake(self):
        """Side channel for the fake gh: `auth token`, porcelain commands."""
        if self.path.startswith("/_fake/stats"):
            with FLIGHT_LOCK:
                return self.respond(200, {"max_in_flight": FLIGHT["max"]})
        length = int(self.headers.get("Content-Length") or 0)
        payload = json.loads(self.rfile.read(length) or b"{}")
        argv = payload.get("argv", [])
        with LOCK:
            state = load_state()
            entry = {"transport": "gh", "method": None, "host": payload.get("host"), "path": None,
                     "query": {}, "operation": None, "variables": None, "body": None, "auth": "gh",
                     "gh_argv": argv, "call": None}
            if self.path.startswith("/_fake/auth"):
                # gh: `gh_auth` (default signed in); glab and tea: `glab_auth` / `tea_auth`
                # (default signed out). A value other than "token"/"missing" is the token itself.
                host = payload.get("host", "")
                cli = payload.get("cli", "gh")
                tokens = state.get(cli + "_auth", {})
                entry["transport"] = cli
                entry["path"] = "auth token" if cli == "gh" else "auth status"
                state.setdefault("requests", []).append(entry)
                save_state(state)
                value = tokens.get(host, tokens.get("*", "token" if cli == "gh" else "missing"))
                if value == "missing":
                    return self.respond(200, {"code": 1, "stdout": "",
                                              "stderr": f"no oauth token found for {host}\n"})
                token = FAKE_TOKEN if value == "token" else value
                return self.respond(200, {"code": 0, "stdout": token + "\n", "stderr": "",
                                          "user": state.get("viewer", "rowan")})
            entry["path"] = "porcelain"
            entry["call"] = argv
            state.setdefault("requests", []).append(entry)
            world = state.get("world") or ARGS.world
            target = Inbox(state, None) if world == "inbox" else Legacy(state, PorcelainRequest(payload.get("host")))
            repo = None
            scope = flag(argv, "--repo")
            if scope.count("/") == 2:
                repo = scope.split("/", 1)[1]
            gate = gate_for(state, entry, repo)
            save_state(state)
        if gate is not None and not wait_gate(gate):
            return self.respond(200, {"code": 1, "stdout": "", "stderr": "Fixture read gate timed out\n"})
        with LOCK:
            state = load_state()
            target.state = state
            code, stdout, stderr = target.porcelain(argv)
            save_state(state)
        self.respond(200, {"code": code, "stdout": stdout, "stderr": stderr})

    def blob(self):
        """Redirect target on another origin: records whether a token leaked across it."""
        with LOCK:
            state = load_state()
            state.setdefault("blob_requests", []).append(
                {"path": self.path, "authorization": "present" if self.headers.get("Authorization") else "absent"})
            save_state(state)
        found = world_actions.blob(self.path.split("?", 1)[0])
        if found:
            content_type, payload = found
            return self.respond(200, payload, {"Content-Type": content_type})
        self.respond(200, "blob payload\n")

    do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = handle_any


def main():
    global ARGS
    parser = argparse.ArgumentParser()
    parser.add_argument("--state", required=True)
    parser.add_argument("--world", default="legacy", choices=["legacy", "inbox", "recorded", "gitlab", "gitea", "forgejo", "actions"])
    parser.add_argument("--fixtures")
    ARGS = parser.parse_args()
    if ARGS.fixtures:
        load_recorded(ARGS.fixtures)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    print(server.server_address[1], flush=True)
    sys.stdout.close()
    os.close(1)
    server.serve_forever()


if __name__ == "__main__":
    main()
