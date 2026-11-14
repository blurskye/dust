#!/usr/bin/env python3
"""A fake `gh` for forge tests (FORGE.md §8.2, F1). Installed as `<dir>/gh` next to a
`fake_gh.json` sidecar (`{"server": "http://127.0.0.1:PORT"}`) naming the fake forge server
(tests/fixtures/forge/server.py). It never touches the network beyond that loopback server.

- `gh auth token --hostname H` prints the fixture token (or fails when the world says the host
  is signed out); `gh auth status` and `gh --version` answer like gh.
- `gh api <path|graphql> --hostname H --method M --include -H ... [--input -]` forwards the
  request to `/{H}/api/v3/<path>` or `/{H}/api/graphql`, marked `X-Fake-Transport: gh`, and prints
  the response in `--include` form; an HTTP error also exits 1 with gh's `gh: msg (HTTP n)`.
- Any other command is porcelain: the server's world answers it (`{code, stdout, stderr}`).
"""
import json
import os
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

SIDECAR = json.loads((pathlib.Path(os.path.realpath(__file__)).parent / "fake_gh.json").read_text())
SERVER = SIDECAR["server"].rstrip("/")


def flag(argv, name, default=None):
    if name in argv:
        index = argv.index(name)
        if index + 1 < len(argv):
            return argv[index + 1]
    return default


def post_json(path, payload):
    request = urllib.request.Request(SERVER + path, data=json.dumps(payload).encode(), method="POST",
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read() or b"{}")


def finish(code, stdout="", stderr=""):
    sys.stdout.write(stdout)
    sys.stderr.write(stderr)
    sys.stdout.flush()
    sys.exit(code)


def host_of(argv):
    target = flag(argv, "--repo") or flag(argv, "-R") or ""
    if target.count("/") == 2:
        return target.split("/", 1)[0]
    return flag(argv, "--hostname") or os.environ.get("GH_HOST") or "github.com"


VALUE_FLAGS = {"--hostname", "--method", "-X", "-H", "--header", "--input", "-f", "-F", "--raw-field", "--field",
               "--jq", "-q", "--template", "-t", "--cache"}


def endpoint_of(argv):
    """`gh api` takes its endpoint as the one positional argument, wherever it sits."""
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


def api(argv):
    endpoint = endpoint_of(argv)
    host = flag(argv, "--hostname") or os.environ.get("GH_HOST") or "github.com"
    method = (flag(argv, "--method") or flag(argv, "-X") or "GET").upper()
    headers = {"X-Fake-Transport": "gh", "X-Fake-Gh-Argv": json.dumps(argv)}
    for index, word in enumerate(argv):
        if word == "-H" and index + 1 < len(argv):
            key, _, value = argv[index + 1].partition(":")
            headers[key.strip()] = value.strip()
    body = sys.stdin.buffer.read() if flag(argv, "--input") == "-" else None
    quoted = urllib.parse.quote(host, safe=":")
    if endpoint == "graphql":
        url = f"{SERVER}/{quoted}/api/graphql"
    else:
        url = f"{SERVER}/{quoted}/api/v3/{endpoint.lstrip('/')}"
    request = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(request, timeout=60)
        status, reason, payload, pairs = response.status, response.reason, response.read(), response.getheaders()
    except urllib.error.HTTPError as error:
        status, reason, payload, pairs = error.code, error.reason, error.read(), list(error.headers.items())
    if "--include" in argv:
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
        sys.stderr.write(f"gh: {message} (HTTP {status})\n")
        sys.exit(1)
    sys.exit(0)


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["--version"]:
        finish(0, "gh version 2.99.0 (fake)\n")
    if argv[:2] == ["auth", "token"]:
        host = flag(argv, "--hostname") or os.environ.get("GH_HOST") or "github.com"
        reply = post_json("/_fake/auth", {"argv": argv, "host": host})
        finish(reply["code"], reply["stdout"], reply["stderr"])
    if argv[:2] == ["auth", "status"]:
        finish(0, "", "github.com\n  Logged in to github.com account tester (keyring)\n")
    if argv[:1] == ["api"]:
        api(argv)
    reply = post_json("/_fake/porcelain", {"argv": argv, "host": host_of(argv)})
    finish(reply["code"], reply["stdout"], reply["stderr"])


if __name__ == "__main__":
    main()
