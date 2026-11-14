#!/usr/bin/env python3
"""A fake `glab` for forge tests (FORGE.md §8.2, F2). Installed as `<dir>/glab` next to a
`fake_glab.json` sidecar (`{"server": "http://127.0.0.1:PORT"}`) naming the fake forge server
(tests/fixtures/forge/server.py). It never touches the network beyond that loopback server.

- `glab --version` answers like glab 1.50 (`FAKE_GLAB_VERSION` overrides the number).
- `glab auth status [--hostname H] [--show-token]` asks the server's `glab_auth` world and prints
  glab's status block on stderr (exit 1 when signed out). The token is only printed with
  `--show-token`, masked otherwise, like glab.
- `glab api <path|graphql> --hostname H -X M --include -H ... [--input -]` forwards the request
  to `/{H}/api/v4/<path>` or `/{H}/api/graphql`, marked `X-Fake-Transport: glab`, and prints the
  response in `--include` form; an HTTP error also exits 1 with `glab: msg (HTTP n)`.
"""
import json
import os
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

SIDECAR = json.loads((pathlib.Path(os.path.realpath(__file__)).parent / "fake_glab.json").read_text())
SERVER = SIDECAR["server"].rstrip("/")
VALUE_FLAGS = {"--hostname", "--method", "-X", "-H", "--header", "--input", "-f", "-F", "--raw-field", "--field"}


def flag(argv, name, default=None):
    if name in argv:
        index = argv.index(name)
        if index + 1 < len(argv):
            return argv[index + 1]
    return default


def finish(code, stdout="", stderr=""):
    sys.stdout.write(stdout)
    sys.stderr.write(stderr)
    sys.stdout.flush()
    sys.exit(code)


def post_json(path, payload):
    request = urllib.request.Request(SERVER + path, data=json.dumps(payload).encode(), method="POST",
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read() or b"{}")


def endpoint_of(argv):
    index = 1
    while index < len(argv):
        word = argv[index]
        if word in VALUE_FLAGS:
            index += 2
            continue
        if not word.startswith("-"):
            return word
        index += 1
    return ""


def auth_status(argv):
    host = flag(argv, "--hostname") or os.environ.get("GITLAB_HOST") or "gitlab.com"
    reply = post_json("/_fake/auth", {"argv": argv, "host": host, "cli": "glab"})
    if reply["code"] != 0:
        finish(1, "", f"x {host} has not been authenticated with glab. "
                      f"Run `glab auth login --hostname {host}`\n")
    token = reply["stdout"].strip()
    shown = token if "--show-token" in argv else "**************************"
    user = reply.get("user", "tester")
    finish(0, "", f"{host}\n"
                  f"  ✓ Logged in to {host} as {user} (/fake/glab-cli/config.yml)\n"
                  f"  ✓ Git operations for {host} configured to use ssh protocol.\n"
                  f"  ✓ API calls for {host} are made over https protocol.\n"
                  f"  ✓ REST API Endpoint: https://{host}/api/v4/\n"
                  f"  ✓ GraphQL Endpoint: https://{host}/api/graphql/\n"
                  f"  ✓ Token: {shown}\n")


def api(argv):
    endpoint = endpoint_of(argv)
    host = flag(argv, "--hostname") or os.environ.get("GITLAB_HOST") or "gitlab.com"
    method = (flag(argv, "--method") or flag(argv, "-X") or "GET").upper()
    headers = {"X-Fake-Transport": "glab", "X-Fake-Gh-Argv": json.dumps(argv)}
    for index, word in enumerate(argv):
        if word in ("-H", "--header") and index + 1 < len(argv):
            key, _, value = argv[index + 1].partition(":")
            headers[key.strip()] = value.strip()
    body = sys.stdin.buffer.read() if flag(argv, "--input") == "-" else None
    quoted = urllib.parse.quote(host, safe=":")
    if endpoint == "graphql":
        url = f"{SERVER}/{quoted}/api/graphql"
    else:
        url = f"{SERVER}/{quoted}/api/v4/{endpoint.lstrip('/')}"
    request = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(request, timeout=60)
        status, reason, payload, pairs = response.status, response.reason, response.read(), response.getheaders()
    except urllib.error.HTTPError as error:
        status, reason, payload, pairs = error.code, error.reason, error.read(), list(error.headers.items())
    if "--include" in argv or "-i" in argv:
        head = f"HTTP/1.1 {status} {reason}\r\n" + "".join(f"{key}: {value}\r\n" for key, value in pairs)
        sys.stdout.buffer.write(head.encode() + b"\r\n" + payload)
    else:
        sys.stdout.buffer.write(payload)
    sys.stdout.flush()
    if status >= 400:
        try:
            message = json.loads(payload).get("message", reason)
        except ValueError:
            message = reason
        sys.stderr.write(f"glab: {message} (HTTP {status})\n")
        sys.exit(1)
    sys.exit(0)


def main():
    argv = sys.argv[1:]
    if argv[:1] in (["--version"], ["version"]):
        finish(0, f"glab {os.environ.get('FAKE_GLAB_VERSION', '1.50.0')} (fake)\n")
    if argv[:2] == ["auth", "status"]:
        auth_status(argv)
    if argv[:1] == ["api"]:
        api(argv)
    finish(1, "", f"fake glab: unsupported command {argv!r}\n")


if __name__ == "__main__":
    main()
