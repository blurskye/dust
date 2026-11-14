#!/usr/bin/env python3
"""A fake `tea` for forge tests (FORGE.md §8.2, F2). Installed as `<dir>/tea` next to a
`fake_tea.json` sidecar (`{"server": "http://127.0.0.1:PORT", "logins": [...]}`) naming the fake
forge server and the logins this tea knows (`{name, url, user, default}`; never a token). It
never touches the network beyond that loopback server.

- `tea --version` answers like tea 0.10.1 (`FAKE_TEA_VERSION` overrides the number).
- `tea login list -o json` prints the sidecar's logins.
- `tea api [--login L] --method M --include <path> [-H ...] [-d @-]` forwards the request to
  `/{H}/api/v1/<path>` for the login's host (the default login when `--login` is absent), marked
  `X-Fake-Transport: tea`, and prints the response in `--include` form. Like tea, an HTTP error
  status still exits 0; an unknown login exits 1.
"""
import json
import os
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

SIDECAR = json.loads((pathlib.Path(os.path.realpath(__file__)).parent / "fake_tea.json").read_text())
SERVER = SIDECAR["server"].rstrip("/")
LOGINS = SIDECAR.get("logins", [])
VALUE_FLAGS = {"--login", "-l", "--method", "-X", "-H", "--header", "-d", "--data", "-f", "-F", "--field"}


def flag(argv, *names):
    for name in names:
        if name in argv:
            index = argv.index(name)
            if index + 1 < len(argv):
                return argv[index + 1]
    return None


def finish(code, stdout="", stderr=""):
    sys.stdout.write(stdout)
    sys.stderr.write(stderr)
    sys.stdout.flush()
    sys.exit(code)


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


def login_for(argv):
    name = flag(argv, "--login", "-l")
    for login in LOGINS:
        if (name and login["name"] == name) or (not name and login.get("default")):
            return login
    return None


def api(argv):
    login = login_for(argv)
    if login is None:
        finish(1, "", "Error: login not found; run `tea login add`\n")
    host = urllib.parse.urlsplit(login["url"]).netloc
    endpoint = endpoint_of(argv)
    method = (flag(argv, "--method", "-X") or "GET").upper()
    headers = {"X-Fake-Transport": "tea", "X-Fake-Gh-Argv": json.dumps(argv)}
    for index, word in enumerate(argv):
        if word in ("-H", "--header") and index + 1 < len(argv):
            key, _, value = argv[index + 1].partition(":")
            headers[key.strip()] = value.strip()
    data = flag(argv, "-d", "--data")
    body = sys.stdin.buffer.read() if data == "@-" else (data.encode() if data else None)
    if body is not None:
        headers.setdefault("Content-Type", "application/json")
    quoted = urllib.parse.quote(host, safe=":")
    url = f"{SERVER}/{quoted}/api/v1/{endpoint.lstrip('/')}"
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
    sys.exit(0)


def main():
    argv = sys.argv[1:]
    if argv[:1] in (["--version"], ["-v"]):
        finish(0, f"Version: {os.environ.get('FAKE_TEA_VERSION', '0.10.1')}\tgolang: 1.22 (fake)\n")
    if argv[:2] == ["login", "list"] or argv[:2] == ["logins", "list"]:
        rows = [{"name": login["name"], "url": login["url"], "user": login.get("user", ""),
                 "default": str(bool(login.get("default"))).lower()} for login in LOGINS]
        finish(0, json.dumps(rows) + "\n")
    if argv[:1] == ["api"]:
        api(argv)
    finish(1, "", f"fake tea: unsupported command {argv!r}\n")


if __name__ == "__main__":
    main()
