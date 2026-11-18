#!/usr/bin/env python3
"""Strict, network-free gh search fixture. Every remote identity includes host/repo/#."""
import json
import os
import pathlib
import sys
import time

args = sys.argv[1:]
path = pathlib.Path(os.environ["DUSTY_INBOX_STATE"])
state = json.loads(path.read_text())
state.setdefault("calls", []).append({"args": args, "host": os.environ.get("GH_HOST")})


def flag(name, default=""):
    return args[args.index(name) + 1] if name in args else default


def save():
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(state))
    temp.replace(path)


def emit(value):
    save()
    print(value if isinstance(value, str) else json.dumps(value))
    sys.exit(0)


def fail(message):
    save()
    print(message, file=sys.stderr)
    sys.exit(1)


host = state.get("host", "github.com")
target = flag("--repo")
if args[:1] and args[0] in ("issue", "pr") and not target:
    fail("Repository command omitted its host-qualified --repo")
repo = "local/root"
if target:
    parts = target.split("/")
    if len(parts) != 3 or parts[0] != host or not all(parts):
        fail("CLI repository host mismatch: " + target)
    repo = "/".join(parts[1:])
if args[:1] == ["api"]:
    if flag("--hostname") != host:
        fail("API request was not host scoped")
    rest = next((arg for arg in args if arg.startswith("repos/")), "")
    if rest:
        repo = "/".join(rest.split("/")[1:3])
    owner = next((arg[6:] for arg in args if arg.startswith("owner=")), "")
    name = next((arg[5:] for arg in args if arg.startswith("name=")), "")
    if owner and name:
        repo = owner + "/" + name
for gate in state.get("gates", []):
    if args[:len(gate["prefix"])] == gate["prefix"] and repo == gate["repo"]:
        save()
        release = pathlib.Path(str(path) + "." + gate["name"])
        deadline = time.monotonic() + 20
        while not release.exists():
            if time.monotonic() > deadline:
                fail("Inbox fixture read gate timed out")
            time.sleep(0.01)

if args[:2] == ["repo", "view"]:
    emit({"nameWithOwner": repo, "url": f"https://{host}/{repo}", "defaultBranchRef": {"name": "main"}})


def item(repository, kind, number=7):
    return {"number": number, "id": f"{repository}-{kind}-{number}",
            "title": f"{repository} item {number}", "body": f"Canonical source for {repository} {kind} {number}",
            "state": ({"merged": "MERGED", "closed": "CLOSED"}.get(state.get("search_state"), "OPEN")
                      if kind == "pr" and repository != "local/root" else "OPEN"), "author": {"login": "alice"}, "labels": [], "assignees": [],
            "createdAt": "2026-11-08T10:00:00Z", "updatedAt": "2026-11-09T10:00:00Z", "commentsCount": 2,
            "repository": {"nameWithOwner": repository},
            "url": f"https://{host}/{repository}/{'issues' if kind == 'issue' else 'pull'}/{number}",
            "headRefOid": "a" * 40, "headRefName": "topic", "baseRefName": "main", "isDraft": False,
            "commits": [], "files": [{"path": repository.split('/')[0] + ".rs", "additions": 1, "deletions": 1}],
            "reviews": [], "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": [],
            "closingIssuesReferences": [], "closedByPullRequestsReferences": []}

if args[:1] == ["search"]:
    if os.environ.get("GH_HOST") != host:
        fail("Search host was not explicitly scoped")
    if "--hostname" in args or "--repo" in args:
        fail("Search received an unsupported host/repository flag")
    if args[1] not in ("prs", "issues"):
        fail("Unknown search type")
    supported = {"number", "title", "body", "state", "author", "labels", "updatedAt", "commentsCount", "repository", "url"}
    if args[1] == "prs":
        supported.add("isDraft")
    fields = flag("--json").split(",")
    if not set(fields) <= supported:
        fail("Search requested unsupported fields: " + repr(set(fields) - supported))
    limit = int(flag("--limit"))
    if not 1 <= limit <= 1000:
        fail("Search was not bounded")
    if str(limit) in state.get("fail_limits", []):
        state["fail_limits"].remove(str(limit))
        fail("Search index/rate-limit unavailable")
    kind = "pr" if args[1] == "prs" else "issue"
    count = state.get("count", 2)
    repos = ["alpha/one", "beta/two"]
    rows = [item(repos[i % 2], kind, 7 + i // 2) for i in range(count)]
    query = args[args.index("--") + 1] if "--" in args else ""
    if "label:empty" in query:
        rows = []
    if state.get("reverse_search"):
        rows.reverse()
    state["search_state"] = "merged" if "--merged" in args else flag("--state", "all")
    for row in rows:
        # Preserve the actual search contract: merged PR search exports closed.
        row["state"] = "closed" if flag("--state") == "closed" or "--merged" in args else "open"
    emit([{key: row[key] for key in fields} for row in rows[:limit]])

if len(args) >= 2 and args[0] in ("issue", "pr"):
    kind = args[0]
    if args[1] == "list":
        emit([item("local/root", kind)])
    if args[1] == "view":
        if "--web" in args:
            state.setdefault("browsers", []).append({"host": host, "repo": repo, "number": int(args[2])})
            emit("")
        emit(item(repo, kind, int(args[2])))
    if args[1] == "diff":
        filename = repo.split('/')[0] + ".rs"
        emit(f"diff --git a/{filename} b/{filename}\n--- a/{filename}\n+++ b/{filename}\n@@ -1 +1 @@\n-old\n+source_for_{repo.split('/')[0]}\n")
    if args[1] == "comment":
        attempt = {"host": host, "repo": repo, "number": int(args[2]), "body": flag("--body"), "kind": kind}
        state.setdefault("write_attempts", []).append(attempt)
        if repo in state.get("fail_comment_repos", []):
            state["fail_comment_repos"].remove(repo)
            fail("Fixture comment write refused")
        state.setdefault("writes", []).append(attempt)
        emit("https://" + host + "/" + repo + "/comment/1")
    if args[1] in ("checkout", "create", "develop"):
        fail("Local Git mutation was attempted by an inbox test")

if args[:1] == ["api"]:
    if flag("--hostname") != host:
        fail("API request was not host scoped")
    if "graphql" in args:
        if "viewerViewedState" in flag("-f"):
            filename = repo.split('/')[0] + ".rs"
            emit([{"data": {"repository": {"pullRequest": {"files": {"nodes": [{"path": filename, "viewerViewedState": "UNVIEWED"}]}}}}}])
        emit([{"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": []}}}}}])
    if any("/timeline" in arg for arg in args):
        emit([[]])
fail("Unexpected fake-gh request: " + repr(args))
