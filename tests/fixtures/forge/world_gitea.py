"""Gitea and Forgejo for the fake forge (`server.py --world gitea|forgejo`, FORGE.md §8.2, F4).

Answers `/api/v1` from the recorded fixtures: `gitea/` (gitea.com, main) for `gitea`,
`forgejo/versions/17/` (code.forgejo.org) for `forgejo`; `state["gitea_fixtures"]` names
another directory under `tests/fixtures/forge/`. Keys are matched the way `forge-record` wrote
them: the sorted query without `limit`/`per_page`; a `page=1` falls back to the unpaged key; a
later page nobody recorded is an empty list (the end of the list). Where a recording holds both
an anonymous 401 and a synthesized signed-in answer, the signed-in one wins. Writes pick the
entry whose recorded body is contained in the request's. `state["answers"]["METHOD path"]`
overrides anything (with or without the query).
"""
import json
import pathlib
import re
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parent
DIRECTORIES = {"gitea": "gitea", "forgejo": "forgejo/versions/17"}
_LOADED = {}


class Reply(Exception):
    def __init__(self, status, body, headers=None):
        super().__init__(status)
        self.status, self.body, self.headers = status, body, headers or {}


def load(directory):
    if directory not in _LOADED:
        base = ROOT / directory
        data = json.loads((base / "requests.json").read_text())
        entries = {}
        for entry in data["entries"]:
            key = entry["key"].split("#", 1)[0].replace("/api/v1/", "/", 1)
            entries.setdefault(key, []).append(entry)
        _LOADED[directory] = (base, data, entries)
    return _LOADED[directory]


def directory_for(state, world):
    return state.get("gitea_fixtures") or DIRECTORIES.get(world, "gitea")


def version(state, world):
    """The recorded `GET version` body, for tests that seed `probes`."""
    base, data, _ = load(directory_for(state, world))
    return {"version": data.get("server_version", "")}


def key_for(request, query):
    pairs = sorted((key, value) for key, value in query.items() if key not in ("per_page", "limit"))
    encoded = urllib.parse.urlencode(pairs)
    return f"{request.method} /{request.rest}" + ("?" + encoded if encoded else "")


def subset(wanted, given):
    if isinstance(wanted, dict):
        return isinstance(given, dict) and all(key in given and subset(value, given[key])
                                               for key, value in wanted.items())
    if isinstance(wanted, list):
        return isinstance(given, list) and len(wanted) == len(given) and all(
            subset(left, right) for left, right in zip(wanted, given))
    return wanted == given


def choose(entries, request):
    if request.method != "GET":
        body = request.json if isinstance(request.json, (dict, list)) else None
        for entry in entries:
            if "request" in entry and body is not None and subset(entry["request"], body):
                return entry
        if body is not None and any("request" in entry for entry in entries):
            return None
        return entries[0]
    signed_in = [entry for entry in entries if entry.get("auth") != "none"]
    return (signed_in or entries)[0]


def reply_for(base, entry):
    body = (base / "bodies" / entry["body"]).read_bytes() if entry.get("body") else b""
    headers = {key: value for key, value in entry.get("headers", {}).items()
               if key.lower() not in ("link", "content-length", "transfer-encoding", "etag",
                                      "content-encoding", "set-cookie")}
    raise Reply(entry["status"], body, headers)


def listed_items(base, entries, owner_repo):
    """Every pull request and issue the recorded lists of `owner_repo` hold, by number."""
    items = {}
    for key, found in entries.items():
        path = key.split(" ", 1)[1].split("?", 1)[0]
        if path not in (f"/repos/{owner_repo}/issues", f"/repos/{owner_repo}/pulls"):
            continue
        for entry in found:
            if entry.get("status") != 200 or not entry.get("body"):
                continue
            body = json.loads((base / "bodies" / entry["body"]).read_bytes())
            for item in body if isinstance(body, list) else []:
                number = item.get("number")
                # A pulls-list object is the fuller pull request; keep it over the issue form.
                if number is not None and ("head" in item or number not in items):
                    items[number] = item
    return items


def listed_runs(base, entries, owner_repo):
    """Every workflow run the recorded `actions/runs` pages of `owner_repo` hold, by id."""
    runs = {}
    for key, found in entries.items():
        if key.split(" ", 1)[1].split("?", 1)[0] != f"/repos/{owner_repo}/actions/runs":
            continue
        for entry in found:
            if entry.get("status") != 200 or not entry.get("body"):
                continue
            body = json.loads((base / "bodies" / entry["body"]).read_bytes())
            for item in body.get("workflow_runs", []) if isinstance(body, dict) else []:
                runs.setdefault(item.get("id"), item)
    return runs


def listed_reply(base, entries, rest):
    """Items the recordings list but never opened: their detail is the listed object and
    their sub-resources (comments, timeline, reviews, files, commits) are empty, so a
    screenshot of any listed row shows real data instead of a 404. Unlisted numbers and
    every other path still 404."""
    run = re.fullmatch(r"repos/([^/]+/[^/]+)/actions/runs/(\d+)(/jobs|/artifacts)?", rest)
    if run:
        # A listed run nobody opened: its detail is the listed object; it lists no jobs or
        # artifacts (true of the skipped ones, and an honest blank for the rest).
        item = listed_runs(base, entries, run.group(1)).get(int(run.group(2)))
        if item is None:
            return
        if run.group(3):
            raise Reply(200, b"[]", {"Content-Type": "application/json"})
        raise Reply(200, json.dumps(item).encode(), {"Content-Type": "application/json"})
    match = re.fullmatch(r"repos/([^/]+/[^/]+)/(pulls|issues)/(\d+)(/[a-z_/]+)?", rest)
    commit = re.fullmatch(r"repos/([^/]+/[^/]+)/commits/([0-9a-f]{7,40})/(status|statuses)", rest)
    json_headers = {"Content-Type": "application/json"}
    if match:
        items = listed_items(base, entries, match.group(1))
        item = items.get(int(match.group(3)))
        if item is None:
            return
        # Issues and pull requests share numbers; `pulls/<an issue>` is a 404 on the forge
        # (a deep link's `#n` probes it).
        if match.group(2) == "pulls" and "head" not in item and not item.get("pull_request"):
            return
        if match.group(4):
            raise Reply(200, b"[]", json_headers)
        raise Reply(200, json.dumps(item).encode(), json_headers)
    if commit:
        heads = {item.get("head", {}).get("sha") for item in listed_items(base, entries, commit.group(1)).values()}
        if commit.group(2) not in heads:
            return
        if commit.group(3) == "statuses":
            raise Reply(200, b"[]", json_headers)
        raise Reply(200, json.dumps({"state": "", "statuses": [], "total_count": 0, "sha": commit.group(2)}).encode(), json_headers)


def answer(state, request, world):
    if state.get("actions_seed") == "design":
        import world_actions
        designed = world_actions.gitea_answer(state, request, world)
        if designed is not None:
            return designed
    if state.get("conversation"):
        conversation(state, request, world)
    recorded(state, request, world)


def conversation(state, request, world):
    """`state["conversation"]`: world_conversation's pull request and issue, built on a
    recorded pull request and issue of the same repository so every field the forge sends is
    there."""
    import world_conversation
    base, data, entries = load(directory_for(state, world))
    owner_repo = data.get("repo", "orbit/orbit")
    items = listed_items(base, entries, owner_repo).values()
    templates = {"pull": next((item for item in items if "head" in item), None),
                 "issue": next((item for item in items if "head" not in item and not item.get("pull_request")), None)}
    if None in templates.values():
        return

    def recorded_reply():
        try:
            recorded(state, request, world)
        except Reply as reply:
            body = reply.body if isinstance(reply.body, bytes) else json.dumps(reply.body).encode()
            return reply.status, body, reply.headers
        return 404, b"", {}

    found = world_conversation.gitea_answer(state, request, world, data.get("host", request.host), owner_repo,
                                            templates, recorded_reply)
    if found is not None:
        raise Reply(*found)


def recorded(state, request, world):
    answers = state.get("answers", {})
    query = dict(request.query)
    for key in (key_for(request, query), f"{request.method} {request.rest}"):
        hit = answers.get(key) or answers.get(key.replace(" /", " ", 1))
        if hit:
            raise Reply(hit.get("status", 200), hit.get("body", {}), hit.get("headers"))
    base, data, entries = load(directory_for(state, world))
    # F6: `since` (incremental refresh) is not recorded: answer the recorded list without it,
    # keeping the entries updated after it.
    since = query.pop("since", None) if request.method == "GET" else None
    # F6: archives are not recorded; like the forge, redirect to storage on another origin
    # (served by server.py's `/_blob/`, one `report.txt` per archive).
    match = re.fullmatch(r"repos/[^/]+/[^/]+/actions/artifacts/(\d+)/zip", request.rest)
    if match and request.method == "GET":
        host = request.headers.get("host", "127.0.0.1").replace("127.0.0.1", "localhost", 1)
        raise Reply(302, b"", {"Location": f"http://{host}/_blob/artifact-{match.group(1)}.zip"})
    found = entries.get(key_for(request, query))
    if not found and query.get("page") == "1":
        found = entries.get(key_for(request, {k: v for k, v in query.items() if k != "page"}))
    if not found and request.method == "GET" and query.get("page") not in (None, "1"):
        first = dict(query, page="1")
        unpaged = {k: v for k, v in query.items() if k != "page"}
        if entries.get(key_for(request, first)) or entries.get(key_for(request, unpaged)):
            raise Reply(200, b"[]", {"Content-Type": "application/json"})
    entry = choose(found, request) if found else None
    if entry is None and request.method == "GET":
        listed_reply(base, entries, request.rest)
    if entry is None:
        host = data.get("host", request.host)
        raise Reply(404, {"message": "404 Not Found", "url": f"https://{host}/api/swagger",
                          "errors": None})
    if since is not None and entry.get("body") and entry["status"] == 200:
        items = json.loads((base / "bodies" / entry["body"]).read_bytes())
        if isinstance(items, list):
            kept = [item for item in items if str(item.get("updated_at", "")) > since]
            raise Reply(200, json.dumps(kept).encode(), {"Content-Type": "application/json"})
    reply_for(base, entry)
