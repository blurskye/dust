#!/usr/bin/env python3
"""GitHub CLI fixture for disposable native-screen tests; never calls the network."""
import json
import os
import pathlib
import sys

args = sys.argv[1:]
state_path = pathlib.Path(os.environ["DUST_GITHUB_FIXTURE"])
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

item = {"id": "NODE_7", "number": 7, "title": "Preserve keyboard drafts", "body": "A real native issue layout.\n\nThe description has multiple paragraphs.", "state": state.get("issue_state", "OPEN"), "author": {"login": "alice"}, "createdAt": "2026-11-08T10:00:00Z", "labels": [{"name": "bug"}], "assignees": [{"login": "bob"}], "milestone": {"title": "v1"}, "projectItems": [{"title": "Native editor"}], "closedByPullRequestsReferences": [{"number": 9, "title": "Native review"}]}
pr = dict(item, id="PR_NODE_9", number=9, title="Native review", baseRefName="main", headRefName="feature/native", headRefOid="a" * 40, isDraft=False, mergeStateStatus="CLEAN", mergeable="MERGEABLE", reviewDecision="APPROVED", reviews=[{"author": {"login": "reviewer"}, "state": "APPROVED", "body": "Reviewed the actual change."}], commits=[{"oid": "a"*40, "messageHeadline": "Keep drafts", "messageBody": "", "committedDate": "2026-11-08"}], files=[{"path": "example.rs", "additions": 1, "deletions": 1}], statusCheckRollup=[{"name": "test (linux)", "conclusion": "SUCCESS", "detailsUrl": "https://github.com/example/demo/actions/runs/91/job/42"}], closingIssuesReferences=[{"number": 7, "title": item["title"]}])
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
if args[:2] == ["repo", "view"]:
    emit({"nameWithOwner": "example/demo", "url": "https://github.com/example/demo", "defaultBranchRef": {"name": "main"}})
if args[:2] == ["issue", "list"]: emit([item])
if args[:2] == ["pr", "list"]: emit([pr])
if args[:2] == ["issue", "view"]: emit(item)
if args[:2] == ["pr", "view"]: emit(pr)
if args[:2] == ["pr", "diff"]:
    emit('diff --git a/example.rs b/example.rs\nindex 1111111..2222222 100644\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\n')
if args[:2] == ["workflow", "view"]: emit(workflow)
if args[:2] == ["run", "list"]: emit([run])
if args[:2] == ["run", "view"]:
    if "--log" in args: emit("test (linux)\tCheck\t2026-11-08T10:00:00Z all native checks passed\n")
    emit(run)
if args and args[0] == "api":
    if "graphql" in args:
        query = next((x for x in args if x.startswith("query=")), "")
        if "reviewThreads" in query:
            emit({"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [{"id": "THREAD_1", "isResolved": state.get("resolved", False), "comments": {"nodes": [{"author": {"login": "reviewer"}, "body": "Please keep this draft.", "path": "example.rs", "line": 1, "originalLine": 1, "diffSide": "RIGHT", "commit": {"oid": "a"*40}}]}}]}}}}})
        if "resolveReviewThread" in query:
            state["resolved"] = "unresolveReviewThread" not in query
        emit({"data": {"saved": True}})
    path = next((arg for arg in args if arg.startswith("repos/")), "")
    if "/commits/" in path:
        emit('diff --git a/example.rs b/example.rs\n--- a/example.rs\n+++ b/example.rs\n@@ -1 +1 @@\n-fn before() {}\n+fn after() {}\n')
    if "timeline" in path:
        events = [{"event": "labeled", "actor": {"login": "alice"}, "label": {"name": "bug"}, "created_at": "2026-11-08"}]
        events.extend({"event": "commented", "user": {"login": "tester"}, "body": body, "created_at": "2026-11-08"} for body in state["comments"])
        emit([events])
    if "actions/workflows" in path:
        emit({"workflows": [{"id": 31, "name": "Native CI", "path": ".github/workflows/native.yml", "state": "active"}]})
    if "/pulls/9/comments" in path: emit({"id": 1})
if args[:2] in (["issue", "comment"], ["pr", "comment"]):
    state["comments"].append(flag("--body"))
    emit("Comment saved")
if args[:2] == ["issue", "close"]: state["issue_state"] = "CLOSED"
if args[:2] == ["issue", "reopen"]: state["issue_state"] = "OPEN"
if args[:2] in (["issue", "close"], ["issue", "reopen"], ["issue", "edit"], ["issue", "create"], ["issue", "develop"], ["pr", "create"], ["pr", "edit"], ["pr", "review"], ["pr", "merge"], ["workflow", "run"], ["run", "cancel"], ["run", "rerun"]):
    emit("Saved fixture operation")
save()
print("Unexpected fixture command: " + repr(args), file=sys.stderr)
sys.exit(1)
