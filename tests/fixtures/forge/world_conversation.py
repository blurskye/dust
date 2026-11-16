"""A real-looking pull request conversation for every world (`state["conversation"]`).

One story, told in each forge's own API shapes: maya's "Clamp exponential backoff to sixty
seconds" closes alex's "Queue stalls after a worker reconnects". The pull request has a long
description, two commits at creation and one pushed after review (plus a force-push), labels,
review requests, an assignee, a milestone, comments from people and two bots (CI results and a
CodeRabbit-style summary, both with `<details><summary>Walkthrough</summary>` and a table), a
long comment, an edited one, reactions, a Changes-requested review with two inline threads (one
answered twice and resolved, one open) and a later approval. The issue has its own discussion,
a bot comment, labels, an assignee and the cross-reference from the pull request.

`state["conversation"]` is `true` or `{"pull": N, "issue": M, "now": EPOCH}`. Numbers default
per world (GitHub #13/#1 like the orbit world, GitLab !15/#16, Gitea #1412/#1413, Forgejo
#1794/#1795); times are relative to `now` (default: when this server started), so screens read
"2 hours ago".

- GitHub: the legacy world's `prs`, `issues`, `timelines` (REST `issues/{n}/timeline`),
  `review_threads` (GraphQL `reviewThreads`) and `diffs` entries.
- GitLab: a merge request and an issue in `state["gitlab"]` with REST notes, discussions and
  `resource_label_events` / `resource_milestone_events`.
- Gitea/Forgejo: answers for `pulls/{n}`, its files, commits, diff, reviews and review comments,
  `issues/{n}`, `issues/{n}/timeline` and `/comments`, the head's status, and the item at the top
  of the recorded lists.

Reactions exist only where the forge sends them with the comment (GitHub); GitLab and Gitea keep
them behind one request per comment, which no provider makes.
"""
import copy
import datetime
import json
import re
import time

STARTED = int(time.time())

TITLE = "Clamp exponential backoff to sixty seconds"
ISSUE_TITLE = "Queue stalls after a worker reconnects"
HEAD_BRANCH = "fix/backoff"
BASE_SHA = "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3"
C1 = "1b9cc6c3a7e4d2f0b8a9c1d2e3f405162738495a"
C2_BEFORE = "5e0d8a41c2b3f4e5d6a7b8c9d0e1f2a3b4c5d6e7"
C2 = "8f3a2c1d9e0b7a6f5e4d3c2b1a09f8e7d6c5b4a3"
C3 = "3c1f9e2a6b5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b"
COMMITS = [
    (C1, 370, "Bound retry exponent before shifting",
     "Bound retry exponent before shifting\n\n`2 ** attempt` grows without limit after a long outage."),
    (C2, 365, "Add backoff regression test", "Add backoff regression test"),
    (C3, 148, "Name the exponent cap and clamp negative attempts",
     "Name the exponent cap and clamp negative attempts\n\nReview: MAX_EXPONENT next to RETRY_LIMIT."),
]
MILESTONE = "v1.4"

DIFF = """diff --git a/orbit/queue.py b/orbit/queue.py
index 1111111..2222222 100644
--- a/orbit/queue.py
+++ b/orbit/queue.py
@@ -10,7 +10,9 @@ import time
 def retry_delay(attempt: int) -> int:
     \"\"\"Return retry delay in seconds.\"\"\"

-    return min(2 ** max(0, attempt), 60)
+    # Cap the exponent before computing the delay.
+    bounded_attempt = min(max(0, attempt), 6)
+    return min(1 << bounded_attempt, 60)


 RETRY_LIMIT = 8
diff --git a/tests/test_queue.py b/tests/test_queue.py
index 3333333..4444444 100644
--- a/tests/test_queue.py
+++ b/tests/test_queue.py
@@ -1,4 +1,5 @@
 from orbit.queue import retry_delay

-def test_delay():
+def test_delay_is_capped():
     assert retry_delay(3) == 8
+    assert retry_delay(40) == 60
"""
FILES = [("orbit/queue.py", 3, 1), ("tests/test_queue.py", 2, 1)]

# Each commit's own change, as `git show` prints it (a forge's per-commit `.diff`); together
# they make DIFF.
COMMIT_DIFFS = {
    C1: """diff --git a/orbit/queue.py b/orbit/queue.py
index 1111111..5555555 100644
--- a/orbit/queue.py
+++ b/orbit/queue.py
@@ -10,7 +10,8 @@ import time
 def retry_delay(attempt: int) -> int:
     \"\"\"Return retry delay in seconds.\"\"\"

-    return min(2 ** max(0, attempt), 60)
+    bounded_attempt = min(attempt, 6)
+    return min(1 << bounded_attempt, 60)


 RETRY_LIMIT = 8
""",
    C2: DIFF[DIFF.index("diff --git a/tests/"):],
    C3: """diff --git a/orbit/queue.py b/orbit/queue.py
index 5555555..2222222 100644
--- a/orbit/queue.py
+++ b/orbit/queue.py
@@ -10,8 +10,9 @@ import time
 def retry_delay(attempt: int) -> int:
     \"\"\"Return retry delay in seconds.\"\"\"

-    bounded_attempt = min(attempt, 6)
+    # Cap the exponent before computing the delay.
+    bounded_attempt = min(max(0, attempt), 6)
     return min(1 << bounded_attempt, 60)


 RETRY_LIMIT = 8
""",
}


def gitlab_diffs(text):
    """A `git diff` as GitLab's per-file `diffs` (`repository/commits/{sha}/diff`)."""
    files = []
    for part in text.split("diff --git a/")[1:]:
        path = part.split(" ", 1)[0]
        hunks = "@@" + part.split("\n@@", 1)[1] if "\n@@" in part else ""
        files.append({"old_path": path, "new_path": path, "a_mode": "100644", "b_mode": "100644",
                      "new_file": False, "renamed_file": False, "deleted_file": False, "diff": hunks})
    return files


def file_diff(path):
    """One file's hunks, as GitLab's `diffs` carry them."""
    part = DIFF.split(f"diff --git a/{path} b/{path}\n", 1)[1].split("\ndiff --git ", 1)[0]
    return "@@" + part.split("\n@@", 1)[1] if "\n@@" in part else part


# The two inline threads: path, new line, the hunk up to that line (GitHub's `diffHunk`).
THREAD_A = ("orbit/queue.py", 14,
            "@@ -10,7 +10,9 @@ import time\n def retry_delay(attempt: int) -> int:\n"
            "     \"\"\"Return retry delay in seconds.\"\"\"\n \n-    return min(2 ** max(0, attempt), 60)\n"
            "+    # Cap the exponent before computing the delay.\n+    bounded_attempt = min(max(0, attempt), 6)")
THREAD_B = ("tests/test_queue.py", 5,
            "@@ -1,4 +1,5 @@\n from orbit.queue import retry_delay\n \n-def test_delay():\n"
            "+def test_delay_is_capped():\n     assert retry_delay(3) == 8\n+    assert retry_delay(40) == 60")

PEOPLE = {
    "maya": ("Maya Ito", "MEMBER", 1001),
    "alex": ("Alex Kim", "MEMBER", 1002),
    "sam": ("Sam Rao", "OWNER", 1003),
    "nina": ("Nina Park", "CONTRIBUTOR", 1004),
    "leo": ("Leo Grant", "FIRST_TIME_CONTRIBUTOR", 1005),
}

# ------------------------------------------------------------------------------ bodies

DESCRIPTION = """## Summary

Retry delays grow as `2 ** attempt`, so a worker that reconnects after a long outage can wait
far longer than the sixty-second ceiling the docs promise. This caps the exponent before the
delay is computed.

## Changes

- Bound the exponent to 6 before shifting, so the delay never passes 64 seconds
- Clamp the final delay to sixty seconds
- Treat negative attempts as the first attempt
- Add a regression test for attempt 40

## How to test

```python
from orbit.queue import retry_delay

assert retry_delay(40) == 60
assert retry_delay(-3) == 1
```

Closes #{issue}. Background in the [retry design notes](https://orbit.example/docs/retries#backoff)."""

CI_BOT = """## Test results

| Suite | Passed | Failed | Skipped | Duration |
|:------|-------:|-------:|--------:|---------:|
| unit | 41 | 0 | 1 | 2.7s |
| integration | 12 | 0 | 0 | 18.4s |
| reconnect-storm | 3 | 0 | 0 | 41.0s |

<details><summary>Walkthrough</summary>

1. `tests/test_queue.py::test_delay_is_capped` covers attempts 3 and 40.
2. The reconnect-storm suite replays 200 reconnects against a local broker.
3. Coverage for `orbit/queue.py` is 96% (+4%).

</details>

<sub>Updated for 8f3a2c1 · [workflow run](https://orbit.example/ci/runs/6)</sub>"""

REVIEW_BOT = """<!-- This is an auto-generated comment: summarize by coderabbit.ai -->
## Summary by CodeRabbit

- **Bug Fixes**
  - Retry delays no longer exceed sixty seconds after a long outage.
- **Tests**
  - Added a regression test for very large attempt counts.

<details><summary>Walkthrough</summary>

The change caps the exponent used by `retry_delay` before the shift, then clamps the result to
the sixty-second ceiling. A new test pins the behaviour for attempt 40.

## Changes

| File | Change summary |
|------|----------------|
| `orbit/queue.py` | Bound the exponent to 6 and clamp the delay to 60 seconds. |
| `tests/test_queue.py` | Renamed the test and added a case for attempt 40. |

</details>

<details><summary>Review details</summary>

**Configuration used**: CodeRabbit UI
**Review profile**: CHILL
**Files selected for processing (2)**: `orbit/queue.py`, `tests/test_queue.py`

</details>

<!-- tips_start -->
Thank you for using CodeRabbit. Comment `@coderabbitai help` to get the list of available commands.
<!-- tips_end -->"""

QUESTION = """Does this also cover the reconnect path in `orbit/worker.py`, or only the queue?

(Edit: I mean `Worker.resume`, which calls `retry_delay` with the attempt count it kept.)"""

ANSWER = """Both: `Worker.resume` goes through `retry_delay`, so it gets the cap too. I'll add a test
for the resume path in a follow-up."""

LONG = """I ran the reconnect storm against staging before and after this change. Notes below,
mostly so we have them next time someone touches the backoff.

### Setup

- 3 brokers, 40 workers, 200 forced disconnects spread over ten minutes
- Each worker keeps its attempt count across reconnects (that is the bug)
- Metrics from the `queue_wait_seconds` histogram, p50 / p95 / max

### Before

| Metric | p50 | p95 | max |
|--------|----:|----:|----:|
| queue wait | 4.1s | 212s | 1,024s |
| reconnect to first task | 2.0s | 64s | 512s |

The max is exactly `2 ** 10`: one worker had failed ten times in a row and then sat idle for
seventeen minutes with tasks waiting.

### After

| Metric | p50 | p95 | max |
|--------|----:|----:|----:|
| queue wait | 3.9s | 58s | 60s |
| reconnect to first task | 1.8s | 31s | 60s |

### What I checked

1. No worker waited longer than sixty seconds.
2. The p50 did not move, so the cap does not make the happy path busier.
3. Broker CPU stayed under 40% during the storm, so we are not trading the stall for a
   thundering herd.

### Open question

The jitter is still applied *after* the clamp:

```python
delay = retry_delay(attempt)
delay += random.uniform(0, delay * 0.1)
```

So the real ceiling is 66 seconds, not 60. I think that is fine (the docs say "about a
minute"), but if we want a hard 60 we should clamp after the jitter instead. Happy to do that
in a separate change so this one stays small.

### Next

- [ ] Follow-up for jitter after the clamp
- [x] Add the storm to the nightly run
- [ ] Update the operations runbook

Thanks for picking this up, it has bitten us twice this quarter."""

ISSUE_BODY = """When a worker reconnects after a broker restart, queued tasks can stay pending for
minutes until another task arrives.

### Steps to reproduce

1. Start two workers against one broker
2. Restart the broker twice within a minute
3. Enqueue a task

### Expected

The task starts within a few seconds.

### Actual

It waits up to seventeen minutes. Logs show `retrying in 1024s`."""

TRIAGE_BOT = """Thanks for the report! Triage summary:

| Field | Value |
|-------|-------|
| Component | `orbit/queue.py` |
| Severity | high (stalls work) |
| Similar issues | #7 (reconnect loses progress) |

<details><summary>Walkthrough</summary>

The issue template's reproduction steps were found and the `bug` label was applied. A
maintainer will confirm the priority.

</details>"""

ISSUE_LOG = """Here is the worker log around the reconnect:

```
09:41:02 worker-2 broker connection lost
09:41:03 worker-2 reconnect attempt 9 failed
09:41:05 worker-2 reconnect attempt 10 failed
09:41:09 worker-2 connected
09:41:09 worker-2 retrying task 8812 in 1024s
```"""

# ------------------------------------------------------------------------------ the story

# (minutes ago, who, body, extras) for the pull request's conversation comments.
PR_COMMENTS = [
    ("ci", 352, CI_BOT, {}),
    ("review-bot", 350, REVIEW_BOT, {}),
    ("nina", 320, QUESTION, {"edited": 318, "reactions": {"+1": 3}}),
    ("maya", 300, ANSWER, {}),
    ("sam", 230, LONG, {"reactions": {"heart": 2, "rocket": 1}}),
    ("leo", 200, "We hit this in production last week; thanks for fixing it so quickly.", {}),
    ("maya", 140, "Pushed `MAX_EXPONENT` and the negative-attempt clamp. @alex could you take another look?", {}),
]
CHANGES_REQUESTED = (240, "A couple of things before this goes in.")
APPROVED = (90, "Thanks, the named cap reads much better. Approving.")
THREAD_A_NOTES = [
    ("alex", 240, "`min(max(0, attempt), 6)` reads as a magic number. Could the 6 be a named constant "
                  "next to `RETRY_LIMIT`?"),
    ("maya", 145, "Good call: `MAX_EXPONENT = 6` in the next push."),
    ("alex", 100, "Thanks, that reads much better."),
]
THREAD_B_NOTES = [
    ("alex", 240, "This only checks the ceiling. Can we add a case for negative attempts too? "
                  "`retry_delay(-1)` returns 1 by accident today."),
]
# `state["conversation"]["suggestion"]`: an open multi-line suggestion on THREAD_A's lines 13–14
# (GitHub only, DESIGN.md §3.30a).
SUGGESTION_NOTES = [
    ("sam", 200, "Name the bound where it is used:\n\n```suggestion\n"
                 "    # Bound the exponent so the delay never passes a minute.\n"
                 "    bounded_attempt = min(max(0, attempt), MAX_EXPONENT)\n```"),
]
ISSUE_COMMENTS = [
    ("ci", 4318, TRIAGE_BOT, {}),
    ("sam", 3990, "Can you share the worker log around the reconnect?", {}),
    ("alex", 3900, ISSUE_LOG, {}),
    ("nina", 2000, "Same here on 0.9.2 after a broker failover.", {"reactions": {"+1": 2}}),
    ("maya", 1500, "Found it: the attempt count survives the reconnect, so the next delay is `2 ** 10`. "
                   "Capping the exponent fixes it.", {"edited": 1490}),
    ("maya", 355, "Fix is up in #{pull}.", {}),
]
ISSUE_OPENED = 4320
# Reactions on the pull request's and the issue's own body (who, GitHub's word).
BODY_REACTIONS = {
    "pull": [("alex", "+1"), ("sam", "+1"), ("nina", "rocket")],
    "issue": [("maya", "+1"), ("sam", "eyes")],
}
GITLAB_AWARDS = {"+1": "thumbsup", "rocket": "rocket", "eyes": "eyes"}
PR_OPENED = 360


def settings(state, world, pull, issue):
    conf = state.get("conversation")
    conf = conf if isinstance(conf, dict) else {}
    now = int(conf.get("now") or STARTED)
    return int(conf.get("pull") or pull), int(conf.get("issue") or issue), now


def stamp(now, minutes, millis=False):
    moment = datetime.datetime.fromtimestamp(now - minutes * 60, datetime.timezone.utc)
    text = moment.strftime("%Y-%m-%dT%H:%M:%S")
    return text + (".000Z" if millis else "Z")


# ------------------------------------------------------------------------------ GitHub

GITHUB_BOTS = {"ci": ("github-actions[bot]", 41898282), "review-bot": ("coderabbitai[bot]", 136622811)}
GITHUB_COLORS = {"bug": "d73a4a", "priority: high": "e4c46a"}


def gh_user(who):
    if who in GITHUB_BOTS:
        login, number = GITHUB_BOTS[who]
        return {"login": login, "id": number, "node_id": f"BOT_{number}", "type": "Bot",
                "html_url": f"https://github.com/apps/{login[:-5]}", "site_admin": False}
    return {"login": who, "id": PEOPLE[who][2], "node_id": f"U_{who}", "type": "User",
            "html_url": f"https://github.com/{who}", "site_admin": False}


def gh_association(who):
    return "NONE" if who in GITHUB_BOTS else PEOPLE[who][1]


def gh_rollup(url, reactions):
    rollup = {"url": url + "/reactions", "total_count": sum(reactions.values())}
    for name in ("+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes"):
        rollup[name] = reactions.get(name, 0)
    return rollup


def gh_groups(reactions):
    words = {"+1": "THUMBS_UP", "-1": "THUMBS_DOWN"}
    return [{"content": words.get(name, name.upper()), "users": {"totalCount": count}}
            for name, count in reactions.items()]


def github(state):
    """Writes the conversation into the legacy world's state (same values on every call)."""
    pull, issue, now = settings(state, "github", 13, 1)
    repo = state.get("repository", "example/demo")
    web = f"https://github.com/{repo}"
    api = f"https://api.github.com/repos/{repo}"
    at = lambda minutes: stamp(now, minutes)
    counter = iter(range(9000, 9999))

    def base(kind, who, minutes, **extra):
        number = next(counter)
        event = {"id": number, "node_id": f"{kind.upper()}_{number}", "url": f"{api}/issues/events/{number}",
                 "actor": gh_user(who), "event": kind, "commit_id": None, "commit_url": None,
                 "created_at": at(minutes), "performed_via_github_app": None}
        event.update(extra)
        return event

    def commented(item, who, minutes, body, extras):
        number = next(counter)
        url = f"{api}/issues/comments/{number}"
        return {"url": url, "html_url": f"{web}/{item}#issuecomment-{number}", "issue_url": f"{api}/issues/{item}",
                "id": number, "node_id": f"IC_{number}", "user": gh_user(who), "created_at": at(minutes),
                "updated_at": at(extras.get("edited", minutes)), "author_association": gh_association(who),
                "body": body, "reactions": gh_rollup(url, extras.get("reactions", {})),
                "performed_via_github_app": None, "event": "commented", "actor": gh_user(who)}

    def reviewed(who, minutes, state_word, body):
        number = next(counter)
        return {"id": number, "node_id": f"PRR_{number}", "user": gh_user(who), "body": body, "commit_id": C2,
                "submitted_at": at(minutes), "state": state_word, "html_url": f"{web}/pull/{pull}#pullrequestreview-{number}",
                "pull_request_url": f"{api}/pulls/{pull}", "author_association": gh_association(who),
                "event": "reviewed"}

    def committed(sha, minutes, message):
        person = {"name": PEOPLE["maya"][0], "email": "maya@orbit.example", "date": at(minutes)}
        return {"sha": sha, "node_id": f"C_{sha[:8]}", "url": f"{api}/git/commits/{sha}",
                "html_url": f"{web}/commit/{sha}", "author": person, "committer": person,
                "tree": {"sha": sha[::-1]}, "message": message, "parents": [],
                "verification": {"verified": False, "reason": "unsigned"}, "event": "committed"}

    def label(name):
        return {"name": name, "color": GITHUB_COLORS[name]}

    events = [committed(C1, COMMITS[0][1], COMMITS[0][3]), committed(C2, COMMITS[1][1], COMMITS[1][3]),
              base("labeled", "maya", 359, label=label("bug")),
              base("labeled", "maya", 359, label=label("priority: high")),
              base("review_requested", "maya", 358, review_requester=gh_user("maya"),
                   requested_reviewer=gh_user("alex")),
              base("review_requested", "maya", 358, review_requester=gh_user("maya"),
                   requested_reviewer=gh_user("sam")),
              base("assigned", "maya", 358, assignee=gh_user("maya"), assigner=gh_user("maya")),
              base("milestoned", "sam", 357, milestone={"title": MILESTONE}),
              base("connected", "maya", 357)]
    for who, minutes, body, extras in PR_COMMENTS:
        events.append(commented(pull, who, minutes, body, extras))
    events.append(reviewed("alex", CHANGES_REQUESTED[0], "changes_requested", CHANGES_REQUESTED[1]))
    events.append(base("head_ref_force_pushed", "maya", 150, commit_id=C2))
    events.append(committed(C3, COMMITS[2][1], COMMITS[2][3]))
    events.append(reviewed("alex", APPROVED[0], "approved", APPROVED[1]))
    events.sort(key=lambda event: event.get("created_at") or event.get("submitted_at") or event["author"]["date"])

    def thread_nodes(notes, path, line, hunk):
        nodes = []
        for index, (who, minutes, body) in enumerate(notes):
            nodes.append({"id": f"PRRC_{path}_{index}", "author": {"login": who, "__typename": "User"},
                          "authorAssociation": gh_association(who), "body": body, "path": path,
                          "line": line, "originalLine": line, "diffSide": "RIGHT", "diffHunk": hunk,
                          "createdAt": at(minutes), "lastEditedAt": None,
                          "commit": {"oid": C2 if minutes > 150 else C3},
                          "reactionGroups": gh_groups({"+1": 1} if index == 0 and path == THREAD_A[0] else {})})
        return {"comments": {"nodes": nodes}}

    threads = [dict({"id": "PRRT_backoff_a", "isResolved": True}, **thread_nodes(THREAD_A_NOTES, *THREAD_A)),
               dict({"id": "PRRT_backoff_b", "isResolved": False}, **thread_nodes(THREAD_B_NOTES, *THREAD_B))]
    conf = state.get("conversation")
    if isinstance(conf, dict) and conf.get("suggestion"):
        suggestion = dict({"id": "PRRT_backoff_s", "isResolved": False, "startLine": THREAD_A[1] - 1},
                          **thread_nodes(SUGGESTION_NOTES, *THREAD_A))
        for node in suggestion["comments"]["nodes"]:
            node["id"] += "_suggestion"
        threads.append(suggestion)

    issue_events = [base("labeled", "alex", ISSUE_OPENED - 1, label=label("bug")),
                    base("labeled", "sam", 4000, label=label("priority: high")),
                    base("assigned", "sam", 4000, assignee=gh_user("maya"), assigner=gh_user("sam")),
                    base("milestoned", "sam", 4000, milestone={"title": MILESTONE})]
    for who, minutes, body, extras in ISSUE_COMMENTS:
        issue_events.append(commented(issue, who, minutes, body.replace("{pull}", str(pull)), extras))
    issue_events.append(dict(base("cross-referenced", "maya", PR_OPENED), source={"type": "issue", "issue": {
        "number": pull, "title": TITLE, "state": "open", "html_url": f"{web}/pull/{pull}",
        "user": gh_user("maya"), "pull_request": {"url": f"{api}/pulls/{pull}", "html_url": f"{web}/pull/{pull}"},
        "repository": {"full_name": repo, "html_url": web}}}))
    issue_events.append(base("connected", "maya", 357))
    issue_events.sort(key=lambda event: event["created_at"])

    comment_rows = [{"author": {"login": gh_user(who)["login"]}, "body": body}
                    for who, _, body, _ in PR_COMMENTS]
    pr = {"number": pull, "id": f"PR_conversation_{pull}", "title": TITLE, "author": {"login": "maya"},
          "body": DESCRIPTION.replace("{issue}", str(issue)), "state": "OPEN",
          "labels": [label("bug"), label("priority: high")], "assignees": [{"login": "maya"}],
          "milestone": {"title": MILESTONE}, "reviewRequests": ["sam"],
          "reviews": [{"author": {"login": "alex"}, "state": "CHANGES_REQUESTED", "body": CHANGES_REQUESTED[1],
                       "submittedAt": at(CHANGES_REQUESTED[0])},
                      {"author": {"login": "alex"}, "state": "APPROVED", "body": APPROVED[1],
                       "submittedAt": at(APPROVED[0])}],
          # Two approvals are required and sam has not reviewed yet.
          "reviewDecision": "REVIEW_REQUIRED", "mergeStateStatus": "BLOCKED", "mergeable": "MERGEABLE", "isDraft": False,
          "commits": [{"oid": sha, "messageHeadline": headline, "messageBody": message.split("\n\n", 1)[1]
                       if "\n\n" in message else "", "committedDate": at(minutes), "authors": [{"login": "maya"}]}
                      for sha, minutes, headline, message in COMMITS],
          "headRefName": HEAD_BRANCH, "baseRefName": "main", "headRefOid": C3,
          "files": [{"path": path, "additions": plus, "deletions": minus} for path, plus, minus in FILES],
          "changedFiles": len(FILES), "additions": 5, "deletions": 2,
          "createdAt": at(PR_OPENED), "updatedAt": at(APPROVED[0]), "projectItems": [],
          "closingIssuesReferences": [{"number": issue, "title": ISSUE_TITLE}], "comments": comment_rows}
    item = {"number": issue, "id": f"I_conversation_{issue}", "title": ISSUE_TITLE, "author": {"login": "alex"},
            "body": ISSUE_BODY, "state": "OPEN", "labels": [label("bug"), label("priority: high")],
            "assignees": [{"login": "maya"}], "milestone": {"title": MILESTONE}, "projectItems": [],
            "createdAt": at(ISSUE_OPENED), "updatedAt": at(355),
            "closedByPullRequestsReferences": [{"number": pull, "title": TITLE}],
            "comments": [{"author": {"login": gh_user(who)["login"]}, "body": body}
                         for who, _, body, _ in ISSUE_COMMENTS]}

    for kind, entry in (("prs", pr), ("issues", item)):
        entries = state.setdefault(kind, [{}])
        found = next((each for each in entries if each.get("number") == entry["number"]), None)
        if found is None:
            entries.append(entry)
        else:
            # A world that already lists the item keeps its place in the list.
            entry.pop("updatedAt")
            found.update(entry)
    state.setdefault("timelines", {})[str(pull)] = events
    state["timelines"][str(issue)] = issue_events
    state.setdefault("review_threads", {})[f"{repo}#{pull}"] = threads
    state.setdefault("diffs", {})[str(pull)] = DIFF
    state.setdefault("commit_diffs", {}).update(COMMIT_DIFFS)


# ------------------------------------------------------------------------------ GitLab

GITLAB_BOTS = {"ci": ("project_6329679_bot_7c2e1d", "orbit CI", 1101),
               "review-bot": ("coderabbit-bot", "CodeRabbit", 1102)}


def gitlab(state, world, gitlab_module):
    """Adds the merge request and issue to the GitLab world once; registers the people."""
    people = {who: {"id": number, "username": who, "name": name} for who, (name, _, number) in PEOPLE.items()}
    people.update({who: {"id": number, "username": login, "name": name}
                   for who, (login, name, number) in GITLAB_BOTS.items()})
    for key, person in people.items():
        gitlab_module.USERS.setdefault(person["username"], person)
    if not any(label["name"] == "priority::high" for label in gitlab_module.LABELS):
        gitlab_module.LABELS.append({"id": 5, "name": "priority::high", "color": "#E4C46A"})
    if world.get("conversation_seeded"):
        return
    world["conversation_seeded"] = True
    pull, issue, now = settings(state, "gitlab", 15, 16)
    at = lambda minutes: stamp(now, minutes, millis=True)
    login = lambda who: people[who]["username"]
    note_ids = iter(range(5000, 5999))
    project, web = gitlab_module.PROJECT, gitlab_module.WEB

    def note(who, minutes, body, system=False, kind=None, position=None, edited=None, noteable="MergeRequest",
             iid=pull):
        value = gitlab_module.note(next(note_ids), login(who), body, at(minutes), system=system, kind=kind,
                                   position=position, resolvable=kind == "DiffNote")
        value.update(updated_at=at(edited if edited is not None else minutes), noteable_type=noteable,
                     noteable_iid=iid, confidential=False, internal=False, attachment=None, commands_changes={})
        return value

    def position(path, line):
        return {"position_type": "text", "base_sha": BASE_SHA, "start_sha": BASE_SHA, "head_sha": C2,
                "old_path": path, "new_path": path, "old_line": None, "new_line": line,
                "line_range": None}

    def thread(notes, path, line, resolved):
        made = [note(who, minutes, body, kind="DiffNote", position=position(path, line))
                for who, minutes, body in notes]
        for each in made:
            each["resolved"] = resolved
            if resolved:
                each.update(resolved_by=gitlab_module.user(login("alex")), resolved_at=at(100))
        return made

    award_ids = iter(range(6000, 6999))

    def awards(which):
        return [{"id": next(award_ids), "name": GITLAB_AWARDS[word], "user": gitlab_module.user(login(who)),
                 "created_at": at(300), "updated_at": at(300), "awardable_type": "MergeRequest"
                 if which == "pull" else "Issue"} for who, word in BODY_REACTIONS[which]]

    thread_a = thread(THREAD_A_NOTES, THREAD_A[0], THREAD_A[1], True)
    thread_b = thread(THREAD_B_NOTES, THREAD_B[0], THREAD_B[1], False)
    commit_note = ("added 1 commit\n\n<ul><li>" + C3[:8] + " - " + COMMITS[2][2] + "</li></ul>\n\n"
                   f"[Compare with previous version](/{project}/-/merge_requests/{pull}/diffs?"
                   f"start_sha={C2}&diff_id=812)")
    notes = [note("maya", 358, "requested review from @alex and @sam", system=True),
             note("maya", 358, "assigned to @maya", system=True)]
    for who, minutes, body, extras in PR_COMMENTS:
        notes.append(note(who, minutes, body, edited=extras.get("edited")))
    notes.append(note("alex", CHANGES_REQUESTED[0], CHANGES_REQUESTED[1]))
    notes.append(note("alex", CHANGES_REQUESTED[0], "requested changes", system=True))
    notes.append(note("maya", COMMITS[2][1], commit_note, system=True))
    notes.append(note("alex", APPROVED[0], APPROVED[1]))
    notes.append(note("alex", APPROVED[0], "approved this merge request", system=True))
    notes.extend(thread_a)
    notes.extend(thread_b)
    notes.sort(key=lambda each: (each["created_at"], each["id"]))
    discussions = [{"id": f"{each['id']:040x}", "individual_note": True, "notes": [each]}
                   for each in notes if each["type"] is None and not each["system"]]
    discussions += [{"id": "a1" * 20, "individual_note": False, "notes": thread_a},
                    {"id": "b2" * 20, "individual_note": False, "notes": thread_b}]
    discussions.sort(key=lambda each: each["notes"][0]["created_at"])

    def label_event(number, who, minutes, name, resource, resource_id):
        found = next(label for label in gitlab_module.LABELS if label["name"] == name)
        return {"id": number, "user": gitlab_module.user(login(who)), "created_at": at(minutes),
                "resource_type": resource, "resource_id": resource_id,
                "label": dict(found, description=None), "action": "add"}

    milestone = dict(gitlab_module.MILESTONES[0], project_id=gitlab_module.PROJECT_ID, description="",
                     created_at="2026-10-01T09:00:00.000Z", updated_at="2026-10-01T09:00:00.000Z",
                     start_date=None, web_url=f"{web}/-/milestones/3")
    mr_id = 400000 + pull
    commits = [{"id": sha, "short_id": sha[:8], "title": headline, "message": message,
                "author_name": PEOPLE["maya"][0], "author_email": "maya@orbit.example",
                "authored_date": at(minutes), "committer_name": PEOPLE["maya"][0],
                "committer_email": "maya@orbit.example", "committed_date": at(minutes), "created_at": at(minutes),
                "parent_ids": [], "web_url": f"{web}/-/commit/{sha}"}
               for sha, minutes, headline, message in reversed(COMMITS)]
    diffs = [{"old_path": path, "new_path": path, "a_mode": "100644", "b_mode": "100644", "new_file": False,
              "renamed_file": False, "deleted_file": False, "diff": file_diff(path)} for path, _, _ in FILES]
    pipeline = gitlab_pipeline(gitlab_module, world, pull, now)
    world.setdefault("commit_diffs", {}).update({sha: gitlab_diffs(text) for sha, text in COMMIT_DIFFS.items()})
    request = gitlab_module.merge_request(
        pull, TITLE, "maya", "opened", at(APPROVED[0]), head_pipeline={k: v for k, v in pipeline.items() if k != "jobs"}, description=DESCRIPTION.replace("{issue}", str(issue)),
        created_at=at(PR_OPENED), reviewers=[gitlab_module.user("alex"), gitlab_module.user("sam")],
        assignees=[gitlab_module.user("maya")], labels=["bug", "priority::high"],
        milestone=gitlab_module.MILESTONES[0], user_notes_count=sum(1 for each in notes if not each["system"]),
        detailed_merge_status="mergeable", source_branch=HEAD_BRANCH, sha=C3,
        diff_refs={"base_sha": BASE_SHA, "head_sha": C3, "start_sha": BASE_SHA},
        approved_by=["alex"], reviewer_states={"alex": "approved", "sam": "unreviewed"}, diffs=diffs,
        commits=commits, discussions=discussions, notes=notes, closes=[issue],
        award_emoji=awards("pull"),
        resource_label_events=[label_event(7001, "maya", 359, "bug", "MergeRequest", mr_id),
                               label_event(7002, "maya", 359, "priority::high", "MergeRequest", mr_id)],
        resource_milestone_events=[{"id": 7101, "user": gitlab_module.user("sam"), "created_at": at(357),
                                    "resource_type": "MergeRequest", "resource_id": mr_id,
                                    "milestone": milestone, "action": "add", "state": "opened"}])
    issue_notes = [note("sam", 4000, "assigned to @maya", system=True, noteable="Issue", iid=issue)]
    for who, minutes, body, extras in ISSUE_COMMENTS:
        issue_notes.append(note(who, minutes, body.replace("#{pull}", f"!{pull}"), edited=extras.get("edited"),
                                noteable="Issue", iid=issue))
    issue_notes.append(note("maya", PR_OPENED, f"mentioned in merge request !{pull}", system=True,
                            noteable="Issue", iid=issue))
    issue_notes.sort(key=lambda each: (each["created_at"], each["id"]))
    issue_id = 500000 + issue
    item = gitlab_module.issue(
        issue, ISSUE_TITLE, "alex", "opened", at(355), description=ISSUE_BODY, created_at=at(ISSUE_OPENED),
        labels=["bug", "priority::high"], assignees=[gitlab_module.user("maya")],
        milestone=gitlab_module.MILESTONES[0], user_notes_count=sum(1 for each in issue_notes if not each["system"]),
        notes=issue_notes, closed_by=[pull], award_emoji=awards("issue"),
        resource_label_events=[label_event(7003, "alex", ISSUE_OPENED - 1, "bug", "Issue", issue_id),
                               label_event(7004, "sam", 4000, "priority::high", "Issue", issue_id)],
        resource_milestone_events=[{"id": 7102, "user": gitlab_module.user("sam"), "created_at": at(4000),
                                    "resource_type": "Issue", "resource_id": issue_id, "milestone": milestone,
                                    "action": "add", "state": "opened"}])
    world["merge_requests"].append(request)
    world["issues"].append(item)


# The merge request's head pipeline: a lint stage and a test stage of two Python versions; the
# reconnect test fails on 3.13 only, so the pipeline failed.
GITLAB_JOBS = [
    # id, name, stage, status, allow_failure, start (seconds after the pipeline), duration
    (88301, "ruff", "lint", "success", False, 4, 19.6),
    (88302, "pytest: [3.12]", "test", "success", False, 27, 61.3),
    (88303, "pytest: [3.13]", "test", "failed", False, 27, 48.9),
]

PYTEST_313 = [
    "\x1b[32;1m$ pip install -e .[test]\x1b[0;m",
    "Successfully installed orbit-1.4.0.dev0 pytest-8.3.3 pytest-asyncio-0.24.0",
    "\x1b[32;1m$ pytest -q --junitxml=report.xml\x1b[0;m",
    "============================= test session starts ==============================",
    "platform linux -- Python 3.13.0, pytest-8.3.3, pluggy-1.5.0",
    "rootdir: /builds/orbit/orbit",
    "configfile: pyproject.toml",
    "plugins: asyncio-0.24.0",
    "collected 44 items",
    "",
    "tests/test_queue.py ............                                         [ 27%]",
    "tests/test_worker.py .........................F......                    [100%]",
    "",
    "=================================== FAILURES ===================================",
    "_________________________ test_reconnect_storm_drains _________________________",
    "",
    "    @pytest.mark.asyncio",
    "    async def test_reconnect_storm_drains():",
    "        broker = FakeBroker(drop_every=5)",
    "        worker = Worker(broker, retry=retry_delay)",
    "        await worker.run_for(seconds=2)",
    ">       assert broker.pending == 0",
    "E       assert 3 == 0",
    "E        +  where 3 = <FakeBroker pending=3 acked=197>.pending",
    "",
    "tests/test_worker.py:118: AssertionError",
    "=============================== warnings summary ===============================",
    "tests/test_worker.py::test_reconnect_storm_drains",
    "  orbit/worker.py:41: DeprecationWarning: asyncio.get_event_loop() without a running loop",
    "    loop = asyncio.get_event_loop()",
    "=========================== short test summary info ============================",
    "FAILED tests/test_worker.py::test_reconnect_storm_drains - assert 3 == 0",
    "=================== 1 failed, 43 passed, 1 warning in 31.72s ===================",
]


def gitlab_trace(name, begun, lines, failed):
    """A GitLab job trace: runner banner, collapsible sections, the script's output."""
    image = "python:3.13-slim" if "3.13" in name else "python:3.12-slim"
    out = ["\x1b[0KRunning with gitlab-runner 18.5.0 (5a021a1c)",
           "\x1b[0K  on orbit-shared-2 xT7kD2pq, system ID: s_4e1a6c09d2b3",
           f"\x1b[0Ksection_start:{begun}:prepare_executor\r\x1b[0K\x1b[0K\x1b[36;1mPreparing the \"docker\" executor\x1b[0;m",
           f"\x1b[0KUsing docker image {image} ...",
           f"\x1b[0Ksection_end:{begun + 6}:prepare_executor\r\x1b[0K",
           f"\x1b[0Ksection_start:{begun + 6}:get_sources\r\x1b[0K\x1b[0K\x1b[36;1mGetting source from Git repository\x1b[0;m",
           f"Checking out {C3[:8]} as detached HEAD (ref is refs/merge-requests/15/head)...",
           f"\x1b[0Ksection_end:{begun + 9}:get_sources\r\x1b[0K",
           f"\x1b[0Ksection_start:{begun + 9}:step_script\r\x1b[0K\x1b[0K\x1b[36;1mExecuting \"step_script\" stage of the job script\x1b[0;m"]
    out += lines
    out.append(f"\x1b[0Ksection_end:{begun + 40}:step_script\r\x1b[0K")
    if failed:
        out.append("\x1b[31;1mERROR: Job failed: exit code 1\x1b[0;m")
    else:
        out.append("\x1b[32;1mJob succeeded\x1b[0;m")
    return "\n".join(out) + "\n"


def gitlab_pipeline(gitlab_module, world, pull, now):
    """Adds the merge request's head pipeline (and its job traces) to the world; returns it."""
    created = now - 148 * 60
    clock = lambda offset: datetime.datetime.fromtimestamp(created + offset, datetime.timezone.utc) \
        .strftime("%Y-%m-%dT%H:%M:%S.000Z")
    jobs, traces = [], world.setdefault("traces", {})
    for job_id, name, stage, status, allowed, start, duration in GITLAB_JOBS:
        each = gitlab_module.job(job_id, name, stage, status, allow_failure=allowed, duration=duration)
        each.update(started_at=clock(start), finished_at=clock(start + int(duration)),
                    created_at=clock(0), ref=f"refs/merge-requests/{pull}/head")
        jobs.append(each)
        if name == "ruff":
            lines = ["\x1b[32;1m$ ruff check orbit tests\x1b[0;m", "All checks passed!",
                     "\x1b[32;1m$ ruff format --check orbit tests\x1b[0;m", "14 files already formatted"]
        elif status == "failed":
            lines = PYTEST_313
        else:
            lines = [line.replace("3.13.0", "3.12.7") for line in PYTEST_313[:11]] + [
                "tests/test_worker.py ................................                    [100%]",
                "",
                "============================= 44 passed in 27.41s =============================="]
        traces[str(job_id)] = gitlab_trace(name, created + start, lines, status == "failed")
    pipeline = gitlab_module.pipeline(88300, 412, f"refs/merge-requests/{pull}/head", "failed", C3,
                                      "merge_request_event", clock(0), jobs)
    pipeline.update(updated_at=clock(27 + 62), duration=27 + 62)
    world["pipelines"].insert(0, pipeline)
    return pipeline


# ------------------------------------------------------------------------------ Gitea / Forgejo

GITEA_NUMBERS = {"gitea": (1412, 1413), "forgejo": (1794, 1795)}
GITEA_ACTIONS = {"gitea": ("gitea-actions", "Gitea Actions"), "forgejo": ("forgejo-actions", "Forgejo Actions")}
GITEA_COLORS = {"bug": "ee0701", "priority: high": "e4c46a"}
# The head's CI jobs on Gitea and Forgejo: name, seconds, steps (name, start, end) and log
# lines, all at second offsets from the job's start.
GITEA_JOBS = [
    ("lint", 21, [("Set up job", 0, 2), ("actions/checkout@v4", 2, 4), ("actions/setup-python@v5", 4, 9),
                  ("ruff check orbit tests", 9, 13), ("ruff format --check orbit tests", 13, 20),
                  ("Complete job", 20, 21)], [
        (0, "Set up job"), (1, "Runner: orbit-runner-1 (act_runner v0.2.13)"),
        (2, "Run actions/checkout@v4"), (4, "Run actions/setup-python@v5 with python-version 3.12"),
        (9, "Run ruff check orbit tests"), (12, "All checks passed!"),
        (13, "Run ruff format --check orbit tests"), (16, "14 files already formatted"),
        (20, "Complete job"),
    ]),
    ("test", 64, [("Set up job", 0, 2), ("actions/checkout@v4", 2, 4), ("actions/setup-python@v5", 4, 9),
                  ("pip install -e .[test]", 9, 32), ("pytest -q", 32, 63), ("Complete job", 63, 64)], [
        (0, "Set up job"), (1, "Runner: orbit-runner-1 (act_runner v0.2.13)"),
        (2, "Run actions/checkout@v4"), (4, "Run actions/setup-python@v5 with python-version 3.12"),
        (9, "Run pip install -e .[test]"), (31, "Successfully installed orbit-1.4.0.dev0 pytest-8.3.3"),
        (32, "Run pytest -q"),
        (33, "============================= test session starts =============================="),
        (33, "platform linux -- Python 3.12.7, pytest-8.3.3, pluggy-1.5.0"),
        (33, "collected 44 items"),
        (34, "tests/test_queue.py ............                                         [ 27%]"),
        (49, "tests/test_worker.py ................................                    [100%]"),
        (61, "============================= 44 passed in 27.41s =============================="),
        (63, "Complete job"),
    ]),
]


_GITEA = {}


def gitea_user(world, host, who):
    if who == "ci":
        login, name = GITEA_ACTIONS.get(world, GITEA_ACTIONS["gitea"])
        number = -2
    elif who == "review-bot":
        login, name, number = "coderabbit-bot", "CodeRabbit", 3101
    else:
        login, name, number = who, PEOPLE[who][0], 2000 + PEOPLE[who][2]
    return {"id": number, "login": login, "login_name": "", "source_id": 0, "full_name": name,
            "email": f"{login}@noreply.{host}", "avatar_url": f"https://{host}/avatars/{number}",
            "html_url": f"https://{host}/{login}", "language": "", "is_admin": False,
            "last_login": "0001-01-01T00:00:00Z", "created": "2024-01-01T00:00:00Z", "restricted": False,
            "active": False, "prohibit_login": False, "location": "", "website": "", "description": "",
            "visibility": "public", "followers_count": 0, "following_count": 0, "starred_repos_count": 0,
            "username": login}


def gitea_data(state, world, host, owner_repo, templates):
    pull, issue, now = settings(state, world, *GITEA_NUMBERS.get(world, GITEA_NUMBERS["gitea"]))
    key = (world, pull, issue, now)
    if key in _GITEA:
        return _GITEA[key]
    at = lambda minutes: stamp(now, minutes)
    web = f"https://{host}/{owner_repo}"
    api = f"https://{host}/api/v1/repos/{owner_repo}"
    person = lambda who: gitea_user(world, host, who)
    ids = iter(range(990000, 999999))
    milestone = {"id": 41, "title": MILESTONE, "description": "", "state": "open", "open_issues": 3,
                 "closed_issues": 9, "created_at": "2026-10-01T09:00:00Z", "updated_at": at(357),
                 "closed_at": None, "due_on": None}

    def label(name, number):
        return {"id": number, "name": name, "exclusive": False, "is_archived": False,
                "color": GITEA_COLORS[name], "description": "", "url": f"{api}/labels/{number}"}

    labels = [label("bug", 61), label("priority: high", 62)]

    def entry(kind, who, minutes, item, **extra):
        number = next(ids)
        value = {"id": number, "type": kind, "html_url": f"{web}/pulls/{item}#issuecomment-{number}",
                 "pull_request_url": f"{web}/pulls/{item}" if item == pull else "",
                 "issue_url": "" if item == pull else f"{web}/issues/{item}", "user": person(who), "body": "",
                 "created_at": at(minutes), "updated_at": at(minutes), "old_project_id": 0, "project_id": 0,
                 "old_milestone": None, "milestone": None, "tracked_time": None, "old_title": "", "new_title": "",
                 "old_ref": "", "new_ref": "", "ref_issue": None, "ref_comment": None, "ref_action": "none",
                 "ref_commit_sha": "", "review_id": 0, "label": None, "assignee": None, "assignee_team": None,
                 "removed_assignee": False, "resolve_doer": None, "dependent_issue": None}
        value.update(extra)
        return value

    def comment(who, minutes, item, body, extras):
        value = entry("comment", who, minutes, item, body=body)
        value["updated_at"] = at(extras.get("edited", minutes))
        return value

    # Reviews: alex's change request, maya's and alex's replies (Gitea files each reply as its own
    # comment review), alex's approval.
    reviews, review_comments = [], {}

    def review(who, minutes, state_word, body, notes):
        number = next(ids)
        reviews.append({"id": number, "user": person(who), "team": None, "state": state_word, "body": body,
                        "commit_id": C2 if minutes > 150 else C3, "stale": False, "official": state_word != "COMMENT",
                        "dismissed": False, "comments_count": len(notes), "submitted_at": at(minutes),
                        "updated_at": at(minutes), "html_url": f"{web}/pulls/{pull}#issuecomment-{number}",
                        "pull_request_url": f"{web}/pulls/{pull}"})
        review_comments[number] = notes
        return number

    def code(who, minutes, body, path, line, hunk, resolved, review_id):
        number = next(ids)
        return {"id": number, "body": body, "user": person(who),
                "resolver": person("alex") if resolved else None, "pull_request_review_id": review_id,
                "created_at": at(minutes), "updated_at": at(minutes), "path": path,
                "commit_id": C2 if minutes > 150 else C3, "original_commit_id": C2, "diff_hunk": hunk,
                "position": line, "original_position": 0,
                "html_url": f"{web}/pulls/{pull}#issuecomment-{number}", "pull_request_url": f"{web}/pulls/{pull}"}

    first_a, first_b = THREAD_A_NOTES[0], THREAD_B_NOTES[0]
    changes = review("alex", CHANGES_REQUESTED[0], "REQUEST_CHANGES", CHANGES_REQUESTED[1], [])
    review_comments[changes] = [code(first_a[0], first_a[1], first_a[2], THREAD_A[0], THREAD_A[1], THREAD_A[2], True, changes),
                                code(first_b[0], first_b[1], first_b[2], THREAD_B[0], THREAD_B[1], THREAD_B[2], False, changes)]
    replies = []
    for who, minutes, body in THREAD_A_NOTES[1:]:
        number = review(who, minutes, "COMMENT", "", [])
        review_comments[number] = [code(who, minutes, body, THREAD_A[0], THREAD_A[1], THREAD_A[2], True, number)]
        replies.append((who, minutes, number))
    approval = review("alex", APPROVED[0], "APPROVED", APPROVED[1], [])
    for each in reviews:
        each["comments_count"] = len(review_comments[each["id"]])

    push = lambda commits, force: json.dumps({"is_force_push": force, "commit_ids": commits}, separators=(",", ":"))
    events = [entry("pull_push", "maya", PR_OPENED, pull, body=push([C1, C2], False)),
              entry("label", "maya", 359, pull, body="1", label=labels[0]),
              entry("label", "maya", 359, pull, body="1", label=labels[1]),
              entry("review_request", "maya", 358, pull, assignee=person("alex")),
              entry("review_request", "maya", 358, pull, assignee=person("sam")),
              entry("assignees", "maya", 358, pull, assignee=person("maya")),
              entry("milestone", "sam", 357, pull, milestone=milestone)]
    comments = []
    for who, minutes, body, extras in PR_COMMENTS:
        value = comment(who, minutes, pull, body, extras)
        events.append(value)
        comments.append(value)
    events.append(entry("review", "alex", CHANGES_REQUESTED[0], pull, body=CHANGES_REQUESTED[1], review_id=changes))
    events += [entry("code", "alex", CHANGES_REQUESTED[0], pull, body=first_a[2], review_id=changes),
               entry("code", "alex", CHANGES_REQUESTED[0], pull, body=first_b[2], review_id=changes)]
    for who, minutes, number in replies:
        events.append(entry("code", who, minutes, pull, body=review_comments[number][0]["body"], review_id=number))
    events.append(entry("pull_push", "maya", 150, pull, body=push([C2_BEFORE, C2], True)))
    events.append(entry("pull_push", "maya", COMMITS[2][1], pull, body=push([C3], False)))
    events.append(entry("review", "alex", APPROVED[0], pull, body=APPROVED[1], review_id=approval))
    events.sort(key=lambda each: (each["created_at"], each["id"]))

    def as_issue(template, number, title, who, body, minutes, updated, is_pull):
        value = copy.deepcopy(template)
        value.update(id=880000 + number, url=f"{api}/issues/{number}", html_url=f"{web}/{'pulls' if is_pull else 'issues'}/{number}",
                     number=number, user=person(who), original_author="", original_author_id=0, title=title,
                     body=body, ref="", assets=[], labels=labels, milestone=milestone,
                     assignee=person("maya"), assignees=[person("maya")], state="open", is_locked=False,
                     comments=len(PR_COMMENTS) if is_pull else len(ISSUE_COMMENTS), created_at=at(minutes),
                     updated_at=at(updated), closed_at=None, due_date=None, time_estimate=0,
                     pull_request={"merged": False, "merged_at": None, "draft": False,
                                   "html_url": f"{web}/pulls/{number}"} if is_pull else None,
                     repository={"id": template.get("repository", {}).get("id", 1), "name": owner_repo.split("/")[1],
                                 "owner": owner_repo.split("/")[0], "full_name": owner_repo})
        return value

    issue_object = as_issue(templates["issue"], issue, ISSUE_TITLE, "alex", ISSUE_BODY, ISSUE_OPENED, 355, False)
    pull_issue = as_issue(templates["issue"], pull, TITLE, "maya", DESCRIPTION.replace("{issue}", str(issue)),
                          PR_OPENED, APPROVED[0], True)
    pull_object = copy.deepcopy(templates["pull"])
    head = copy.deepcopy(pull_object.get("head") or {})
    base = copy.deepcopy(pull_object.get("base") or {})
    head.update(label=HEAD_BRANCH, ref=HEAD_BRANCH, sha=C3, repo_id=base.get("repo_id", head.get("repo_id")),
                repo=base.get("repo", head.get("repo")))
    base.update(label="main", ref="main", sha=BASE_SHA)
    pull_object.update(
        id=870000 + pull, url=f"{web}/pulls/{pull}", number=pull, user=person("maya"), title=TITLE,
        body=pull_issue["body"], labels=labels, milestone=milestone, assignee=person("maya"),
        assignees=[person("maya")], requested_reviewers=[person("sam")], requested_reviewers_teams=[],
        state="open", draft=False, is_locked=False, comments=len(PR_COMMENTS), review_comments=4,
        additions=5, deletions=2, changed_files=len(FILES), html_url=f"{web}/pulls/{pull}",
        diff_url=f"{web}/pulls/{pull}.diff", patch_url=f"{web}/pulls/{pull}.patch", mergeable=True,
        merged=False, merged_at=None, merge_commit_sha=None, merged_by=None, allow_maintainer_edit=True,
        base=base, head=head, merge_base=BASE_SHA, due_date=None, created_at=at(PR_OPENED),
        updated_at=at(APPROVED[0]), closed_at=None, pin_order=0)

    issue_events = [entry("label", "alex", ISSUE_OPENED - 1, issue, body="1", label=labels[0]),
                    entry("label", "sam", 4000, issue, body="1", label=labels[1]),
                    entry("assignees", "sam", 4000, issue, assignee=person("maya")),
                    entry("milestone", "sam", 4000, issue, milestone=milestone)]
    issue_comments = []
    for who, minutes, body, extras in ISSUE_COMMENTS:
        value = comment(who, minutes, issue, body.replace("{pull}", str(pull)), extras)
        issue_events.append(value)
        issue_comments.append(value)
    ref = {key: pull_issue[key] for key in ("id", "url", "html_url", "number", "user", "title", "state",
                                             "pull_request", "repository", "created_at", "updated_at")}
    issue_events.append(entry("pull_ref", "maya", PR_OPENED, issue, ref_issue=ref, ref_action="closes"))
    issue_events.sort(key=lambda each: (each["created_at"], each["id"]))

    def plain(value):
        return {key: value[key] for key in ("id", "html_url", "pull_request_url", "issue_url", "user", "body",
                                            "created_at", "updated_at")} | {"original_author": "",
                                                                            "original_author_id": 0, "assets": []}

    person_commit = lambda minutes: {"name": PEOPLE["maya"][0], "email": "maya@orbit.example", "date": at(minutes)}
    commits = [{"url": f"{api}/git/commits/{sha}", "sha": sha, "created": at(minutes), "html_url": f"{web}/commit/{sha}",
                "commit": {"url": f"{api}/git/commits/{sha}", "author": person_commit(minutes),
                           "committer": person_commit(minutes), "message": message,
                           "tree": {"url": f"{api}/git/trees/{sha[::-1]}", "sha": sha[::-1], "created": at(minutes)},
                           "verification": {"verified": False, "reason": "gpg.error.not_signed_commit",
                                            "signature": "", "signer": None, "payload": ""}},
                "author": person("maya"), "committer": person("maya"), "parents": [], "files": None, "stats": None}
               for sha, minutes, _, message in COMMITS]
    files = [{"filename": path, "status": "changed", "additions": plus, "deletions": minus, "changes": plus + minus,
              "html_url": f"{web}/src/commit/{C3}/{path}", "contents_url": f"{api}/contents/{path}?ref={C3}",
              "raw_url": f"{web}/raw/commit/{C3}/{path}", "previous_filename": ""} for path, plus, minus in FILES]
    # The head's CI run: `ci.yml` with a lint and a test job. Gitea links a status to its run
    # by the run's index in the repository and to its job by its place in the run
    # (`…/runs/6/jobs/1`); the API reads the run by its id, which differs (4407).
    run_id, run_number, started = 4407, 6, 142
    statuses, jobs, logs = [], [], {}
    for index, (name, seconds, job_steps, lines) in enumerate(GITEA_JOBS):
        job_id = 6101 + index
        begun, ended = now - started * 60 + index * 3, now - started * 60 + index * 3 + seconds
        clock = lambda offset, begun=begun: datetime.datetime.fromtimestamp(
            begun + offset, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        steps = [{"number": number, "name": step, "status": "completed", "conclusion": "success",
                  "started_at": clock(start), "completed_at": clock(end)}
                 for number, (step, start, end) in enumerate(job_steps, 1)]
        jobs.append({"id": job_id, "run_id": run_id, "run_attempt": 1, "name": name, "status": "completed",
                     "conclusion": "success", "head_branch": HEAD_BRANCH, "head_sha": C3,
                     "labels": ["ubuntu-latest"], "runner_id": 3, "runner_name": "orbit-runner-1",
                     "created_at": clock(0), "started_at": clock(0), "completed_at": clock(seconds),
                     "steps": steps, "html_url": f"{web}/actions/runs/{run_number}/jobs/{index}",
                     "url": f"{api}/actions/jobs/{job_id}", "run_url": f"{api}/actions/runs/{run_id}"})
        logs[f"actions/jobs/{job_id}/logs"] = "".join(f"{clock(offset)} {line}\n" for offset, line in lines)
        statuses.append({"context": f"ci / {name} (pull_request)", "state": "success",
                         "target_url": f"{web}/actions/runs/{run_number}/jobs/{index}",
                         "description": f"Successful in {seconds // 60}m{seconds % 60}s" if seconds >= 60
                         else f"Successful in {seconds}s",
                         "id": 77001 + index, "url": "", "created_at": clock(0), "updated_at": clock(seconds),
                         "creator": person("ci")})
    run = {"id": run_id, "run_number": run_number, "display_title": TITLE, "event": "pull_request",
           "status": "completed", "conclusion": "success", "head_branch": HEAD_BRANCH, "head_sha": C3,
           "path": "ci.yml@refs/pull/{}/head".format(pull), "run_attempt": 1,
           "started_at": jobs[0]["started_at"], "completed_at": max(job["completed_at"] for job in jobs),
           "html_url": f"{web}/actions/runs/{run_number}", "url": f"{api}/actions/runs/{run_id}",
           "actor": person("maya"), "trigger_actor": person("maya"), "repository_id": 4242}
    data = {
        "pull": pull, "issue": issue,
        f"pulls/{pull}": pull_object, f"issues/{pull}": pull_issue, f"issues/{issue}": issue_object,
        f"issues/{pull}/timeline": events, f"issues/{pull}/comments": [plain(each) for each in comments],
        f"issues/{issue}/timeline": issue_events, f"issues/{issue}/comments": [plain(each) for each in issue_comments],
        f"pulls/{pull}/reviews": reviews, f"pulls/{pull}/files": files, f"pulls/{pull}/commits": commits,
        f"commits/{C3}/status": {"state": "success", "statuses": statuses, "total_count": len(statuses),
                                 "sha": C3, "commit_url": f"{api}/commits/{C3}", "url": f"{api}/commits/{C3}/status"},
        f"commits/{C3}/statuses": statuses,
        f"actions/runs/{run_id}": run,
        f"actions/runs/{run_id}/jobs": {"total_count": len(jobs), "jobs": jobs},
        f"issues/{pull}/reactions": [{"user": person(who), "content": word, "created_at": at(300)}
                                     for who, word in BODY_REACTIONS["pull"]],
        f"issues/{issue}/reactions": [{"user": person(who), "content": word, "created_at": at(300)}
                                      for who, word in BODY_REACTIONS["issue"]],
        "texts": dict(logs, **{f"git/commits/{sha}.diff": diff for sha, diff in COMMIT_DIFFS.items()}),
    }
    for job in jobs:
        data[f"actions/jobs/{job['id']}"] = job
    data["head_runs"] = {"workflow_runs": [run], "total_count": 1}
    for number, notes in review_comments.items():
        data[f"pulls/{pull}/reviews/{number}/comments"] = notes
    _GITEA[key] = data
    return data


def gitea_answer(state, request, world, host, owner_repo, templates, recorded):
    """The conversation's answer for this request, or None. `recorded()` gives the recorded
    reply (status, body bytes, headers) for lists the conversation joins."""
    if request.method != "GET" or not request.rest.startswith(f"repos/{owner_repo}/"):
        return None
    tail = request.rest[len(f"repos/{owner_repo}/"):]
    data = gitea_data(state, world, host, owner_repo, templates)
    json_headers = {"Content-Type": "application/json"}
    if tail == f"pulls/{data['pull']}.diff":
        return 200, DIFF.encode(), {"Content-Type": "text/plain; charset=utf-8"}
    if tail == "actions/runs" and request.query.get("head_sha") == C3:
        return 200, json.dumps(data["head_runs"]).encode(), json_headers
    if tail in data["texts"]:
        return 200, data["texts"][tail].encode(), {"Content-Type": "text/plain; charset=utf-8"}
    if tail in data:
        page = request.query.get("page", "1")
        body = data[tail] if page in ("", "1") or not isinstance(data[tail], list) else []
        return 200, json.dumps(body).encode(), json_headers
    if tail in ("pulls", "issues") and request.query.get("page", "1") in ("", "1"):
        wanted = request.query.get("state", "open")
        # Only the plain lists: a filtered one (labels, search, poster) would not match.
        if wanted not in ("open", "all") or set(request.query) - {"state", "type", "page", "limit", "per_page",
                                                                  "sort", "since"}:
            return None
        status, body, headers = recorded()
        if status != 200:
            return None
        items = json.loads(body)
        if not isinstance(items, list):
            return None
        kind = "pulls" if tail == "pulls" else request.query.get("type", "")
        added = []
        if kind == "pulls" and tail == "pulls":
            added.append(data[f"pulls/{data['pull']}"])
        elif kind in ("pulls", ""):
            added.append(data[f"issues/{data['pull']}"])
        if kind in ("issues", ""):
            added.append(data[f"issues/{data['issue']}"])
        known = {item.get("number") for item in items}
        items = [item for item in added if item["number"] not in known] + items
        return 200, json.dumps(items).encode(), dict(headers, **json_headers)
    return None
