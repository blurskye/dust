#!/usr/bin/env python3
"""GitHub CLI fixture for disposable native-screen tests; never calls the network."""
import json
import os
import pathlib
import sys
import time

args = sys.argv[1:]
state_path = pathlib.Path(os.environ["DUSTY_GITHUB_FIXTURE"])
state = json.loads(state_path.read_text()) if state_path.exists() else {"calls": [], "comments": []}
state["calls"].append(args)

def flag(name, default=""):
    return args[args.index(name) + 1] if name in args else default

def save():
    temporary = state_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state))
    temporary.replace(state_path)

def emit(value):
    save()
    print(value if isinstance(value, str) else json.dumps(value))
    sys.exit(0)

# Log a controlled slow read before waiting, so native-input tests can navigate
# while it is active and release it without racing writes to the fixture state.
for gate in state.get("read_gates", []):
    if ("prefix" in gate and args[:len(gate["prefix"])] == gate["prefix"]
            or "contains" in gate and any(gate["contains"] in arg for arg in args)):
        save()
        release = state_path.with_name(state_path.name + "." + gate["name"])
        deadline = time.monotonic() + 20
        while not release.exists():
            if time.monotonic() >= deadline:
                print("Fixture read gate timed out: " + gate["name"], file=sys.stderr)
                sys.exit(1)
            time.sleep(0.01)

item = {"id": "NODE_7", "number": 7, "title": "Preserve keyboard drafts", "body": "A real native issue layout.\n\nThe description has multiple paragraphs.", "state": state.get("issue_state", "OPEN"), "author": {"login": "alice"}, "createdAt": "2026-11-08T10:00:00Z", "comments": [{"body": body} for body in state.get("comments", [])], "labels": [{"name": "bug"}], "assignees": [{"login": "bob"}], "milestone": {"title": "v1"}, "projectItems": [{"title": "Native editor"}], "closedByPullRequestsReferences": [{"number": 9, "title": "Native review"}]}
pr = dict(item, id="PR_NODE_9", number=9, title="Native review", baseRefName="main", headRefName="feature/native", headRefOid="a" * 40, isDraft=False, mergeStateStatus="CLEAN", mergeable="MERGEABLE", reviewDecision="APPROVED", reviews=[{"author": {"login": "reviewer"}, "state": "APPROVED", "body": "Reviewed the actual change."}], commits=[{"oid": "a"*40, "messageHeadline": "Keep drafts", "messageBody": "", "committedDate": "2026-11-08"}], changedFiles=2, files=[{"path": "example.rs", "additions": 1, "deletions": 1}, {"path": "src/nested/other.rs", "additions": 1, "deletions": 0}], statusCheckRollup=[{"name": "test (linux)", "conclusion": "SUCCESS", "detailsUrl": "https://github.com/example/demo/actions/runs/91/job/42"}], closingIssuesReferences=[{"number": 7, "title": item["title"]}])
workflow = """name: Native CI
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
run = {"databaseId": 91, "displayTitle": "Validate native workspace", "workflowName": "Native CI", "workflowDatabaseId": 31, "headBranch": "main", "headSha": "a"*40, "event": "push", "attempt": 2, "status": "completed", "conclusion": "success", "jobs": [{"databaseId": 41, "name": "build", "status": "completed", "conclusion": "success", "steps": [{"name": "Build", "number": 1, "status": "completed", "conclusion": "success"}]}, {"databaseId": 42, "name": "test (linux)", "status": "completed", "conclusion": "success", "steps": [{"name": "Check", "number": 1, "status": "completed", "conclusion": "success"}]}, {"databaseId": 43, "name": "test (windows)", "status": "completed", "conclusion": "success", "steps": []}]}
if args[:2] == ["run", "view"] and "--log" not in args:
    state["run_views"] = state.get("run_views", 0) + 1
if state.get("run_views", 0) < 2:
    run.update(status="in_progress", conclusion="")
    run["jobs"][1].update(status="in_progress", conclusion="")
if args[:2] == ["repo", "view"]:
    target = flag("--repo")
    if target:
        host, name = target.split("/", 1)
    else:
        host, name = state.get("host", "github.com"), state.get("repository", "example/demo")
    emit({"nameWithOwner": name, "url": "https://" + host + "/" + name, "defaultBranchRef": {"name": "main"}})
if len(args) >= 2 and args[0] in ("issue", "pr") and args[1] in ("list", "view"):
    kind = args[0]
    default = item if kind == "issue" else pr
    entries = [dict(default, **entry) for entry in state.get(kind + "s", [default])]
    if args[1] == "list":
        failures = state.get("fail_list_limits", {})
        limit = flag("--limit", "100")
        if failures.get(limit, 0):
            failures[limit] -= 1
            save()
            print("Fixture list read refused", file=sys.stderr)
            sys.exit(1)
        overrides = state.get(kind + "_queries", {}).get(flag("--search"))
        if overrides is not None:
            entries = [dict(default, **entry) for entry in overrides]
        wanted_state = flag("--state", "open").upper()
        if wanted_state != "ALL":
            entries = [entry for entry in entries if entry["state"] == wanted_state]
        entries = entries[:int(limit)]
        fields = flag("--json").split(",")
        omitted = state.get("list_omit_fields", [])
        emit([] if state.get("empty") else [{key: entry[key] for key in fields if key in entry and key not in omitted} for entry in entries])
    if state.get("fail_item_views", 0):
        state["fail_item_views"] -= 1
        save()
        print("Fixture refresh refused", file=sys.stderr)
        sys.exit(1)
    emit(next((entry for entry in entries if entry["number"] == int(args[2])), default))
if args[:2] == ["pr", "diff"]:
    emit(state.get("diffs", {}).get(args[2], state.get("diff", 'diff --git a/example.rs b/example.rs\nindex 1111111..2222222 100644\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\ndiff --git a/src/nested/other.rs b/src/nested/other.rs\n--- /dev/null\n+++ b/src/nested/other.rs\n@@ -0,0 +1 @@\n+fn second() {}\n')))
if args[:2] == ["workflow", "view"]: emit(workflow)
if args[:2] == ["run", "list"]: emit([run])
if args[:2] == ["run", "view"]:
    if "--log" in args:
        emit(state.get("logs", {}).get(flag("--job"), state.get("log",
             "test (linux)\tUNKNOWN STEP\t\ufeff2026-11-08T09:59:58.100Z Current runner version: fixture\n"
             "test (linux)\tUNKNOWN STEP\t2026-11-08T10:00:00.200Z \x1b[32mall native checks passed\x1b[0m\n")))
    for job in run["jobs"]:
        for step in job["steps"]:
            step.update(startedAt="2026-11-08T10:00:00Z", completedAt="2026-11-08T10:00:01Z")
    emit(run)
if args and args[0] == "api":
    if "graphql" in args:
        query = next((x for x in args if x.startswith("query=")), "")
        values = dict(arg.split("=", 1) for arg in args if "=" in arg)
        if "reviewThreads" in query:
            assert "originalLine diffSide" not in query, "diffSide is not a PullRequestReviewComment field"
            assert "after:$endCursor" in query and "--paginate" in args and "--slurp" in args, "review threads must paginate"
            source = values["owner"] + "/" + values["name"] + "#" + values["number"]
            comments = [{"author": {"login": "reviewer"}, "body": "Please keep this draft.", "path": "example.rs", "line": 1, "originalLine": 1, "commit": {"oid": "a"*40}}]
            threads = json.loads(json.dumps(state.get("review_threads", {}).get(source, [{"id": "THREAD_1", "isResolved": state.get("resolved", False), "comments": {"nodes": comments}}])))
            for thread in threads:
                thread["comments"]["nodes"].extend(state.get("thread_replies", {}).get(thread["id"], []))
                thread["isResolved"] = state.get("thread_resolved", {}).get(thread["id"], thread.get("isResolved", False))
            threads.extend({"id": "INLINE_" + str(index), "isResolved": state.get("thread_resolved", {}).get("INLINE_" + str(index), False), "comments": {"nodes": [comment]}} for index, comment in enumerate(state.get("inline_comments", [])))
            # Two pages exercise the native page-flattening path, including empty pages.
            pages = [threads[:1], threads[1:]]
            emit([{"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": page, "pageInfo": {"hasNextPage": index == 0, "endCursor": "next" if index == 0 else None}}}}}} for index, page in enumerate(pages)])
        if "addPullRequestReviewThreadReply" in query:
            assert "pullRequestReviewThreadId:$thread" in query, "reply must preserve the original thread"
            assert "path" not in values and "line" not in values and "commit_id" not in values, "a reply must not create a new inline conversation"
            if state.get("fail_reply", 0):
                state["fail_reply"] -= 1
                save()
                print("GitHub rejected the thread reply", file=sys.stderr)
                sys.exit(1)
            thread = values["thread"]
            comment = {"author": {"login": "rowan"}, "body": values["body"], "path": "example.rs", "line": 1, "originalLine": 1}
            state.setdefault("thread_replies", {}).setdefault(thread, []).append(comment)
            state.setdefault("reply_targets", []).append({"host": flag("--hostname"), "repository": values["owner"] + "/" + values["name"], "thread": thread, "body": values["body"]})
            emit({"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "REPLY_NODE_1"}}}})
        if "viewerViewedState" in query:
            emit([{"data": {"repository": {"pullRequest": {"files": {"nodes": [{"path": file["path"], "viewerViewedState": "VIEWED" if file["path"] in state.get("viewed", []) else "UNVIEWED"} for file in pr["files"]], "pageInfo": {"hasNextPage": False, "endCursor": None}}}}}}])
        if "markFileAsViewed" in query:
            path = next(arg.removeprefix("path=") for arg in args if arg.startswith("path="))
            viewed = set(state.get("viewed", []))
            if "unmarkFileAsViewed" in query:
                viewed.discard(path)
            else:
                viewed.add(path)
            state["viewed"] = sorted(viewed)
        if "resolveReviewThread" in query:
            state.setdefault("thread_resolved", {})[values["thread"]] = "unresolveReviewThread" not in query
        emit({"data": {"saved": True}})
    path = next((arg for arg in args if arg.startswith("repos/")), "")
    if path.count("/") == 2:
        name = path.removeprefix("repos/")
        emit(state.get("repo_info", {}).get(name, {"full_name": name, "default_branch": "main"}))
    if path.endswith("/pulls") and flag("--method") == "POST":
        if state.get("fail_create"):
            state["fail_create"] -= 1
            save()
            print("GitHub rejected PR creation", file=sys.stderr)
            sys.exit(1)
        emit({"number": 9, "html_url": "https://github.com/" + path.removeprefix("repos/").removesuffix("/pulls") + "/pull/9"})
    if "/commits/" in path:
        if state.get("fail_commit_views", 0):
            state["fail_commit_views"] -= 1
            save()
            print("Fixture commit refresh refused", file=sys.stderr)
            sys.exit(1)
        emit('diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\n')
    if "timeline" in path:
        events = [{"event": "labeled", "actor": {"login": "alice"}, "label": {"name": "bug"}, "created_at": "2026-11-08"}]
        events.extend({"event": "commented", "user": {"login": "rowan"}, "body": body, "created_at": "2026-11-08"} for body in state["comments"])
        emit([events])
    if "actions/workflows" in path:
        emit({"workflows": [{"id": 31, "name": "Native CI", "path": ".github/workflows/native.yml", "state": "active"}]})
    if "/pulls/9/comments" in path:
        values = dict(arg.split("=", 1) for arg in args if "=" in arg)
        state.setdefault("inline_comments", []).append({"author": {"login": "rowan"}, "body": values["body"], "path": values["path"], "line": int(values["line"]), "originalLine": int(values["line"]), "diffSide": values["side"], "commit": {"oid": values["commit_id"]}})
        emit({"id": 1, "node_id": "COMMENT_NODE_1", "body": values["body"]})
if args[:2] in (["issue", "comment"], ["pr", "comment"]):
    if state.get("fail_comments", 0):
        state["fail_comments"] -= 1
        save()
        print("GitHub rejected the comment", file=sys.stderr)
        sys.exit(1)
    state["comments"].append(flag("--body"))
    emit("Comment saved")
if args[:2] == ["issue", "close"]: state["issue_state"] = "CLOSED"
if args[:2] == ["issue", "reopen"]: state["issue_state"] = "OPEN"
if args[:2] in (["issue", "close"], ["issue", "reopen"], ["issue", "edit"], ["issue", "create"], ["issue", "develop"], ["pr", "create"], ["pr", "edit"], ["pr", "review"], ["pr", "merge"], ["workflow", "run"], ["run", "cancel"], ["run", "rerun"]):
    emit("Saved fixture operation")
save()
print("Unexpected fixture command: " + repr(args), file=sys.stderr)
sys.exit(1)
