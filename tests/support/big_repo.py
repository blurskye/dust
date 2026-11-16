#!/usr/bin/env python3
"""A synthetic big repository for the Git tab's speed tests (tests/git_speed.rs).

    python3 big_repo.py DIR [--commits N] [--files N] [--changes N] [--untracked N]
                            [--branches N] [--stashes N] [--wide N]

History is written with `git fast-import` (one commit adds every file, each later commit edits
one file, the commit 10 below the tip edits `--wide` files at once), then `--branches` branches
spread over the history, `--stashes` stashes, and `--changes` edited tracked files plus
`--untracked` new files in the worktree. Subjects are `commit N`, N from 1 (the root) up.
"""
import argparse
import os
import subprocess


def path(index):
    return f"src/d{index // 100:03d}/f{index:05d}.txt"


def content(index, revision):
    return f"file {index}\nrevision {revision}\n" + "".join(f"line {line}\n" for line in range(8))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("dir")
    parser.add_argument("--commits", type=int, default=50_000)
    parser.add_argument("--files", type=int, default=20_000)
    parser.add_argument("--changes", type=int, default=3_000)
    parser.add_argument("--untracked", type=int, default=50)
    parser.add_argument("--branches", type=int, default=300)
    parser.add_argument("--stashes", type=int, default=50)
    parser.add_argument("--wide", type=int, default=2_000)
    args = parser.parse_args()

    env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0")

    def git(*command, **options):
        return subprocess.run(["git", "-C", args.dir, *command], check=True, env=env, **options)

    os.makedirs(args.dir, exist_ok=True)
    git("init", "-q", "-b", "main")
    for key, value in [("user.name", "Big Repo"), ("user.email", "big@example.invalid"),
                       ("commit.gpgsign", "false"), ("gc.auto", "0")]:
        git("config", key, value)

    stream = subprocess.Popen(["git", "-C", args.dir, "fast-import", "--quiet"],
                              stdin=subprocess.PIPE, env=env)
    out = []

    def data(text):
        raw = text.encode()
        out.append(b"data %d\n" % len(raw))
        out.append(raw)
        out.append(b"\n")

    def flush():
        stream.stdin.write(b"".join(out))
        out.clear()

    start = 1_600_000_000
    wide_at = max(2, args.commits - 10)
    for number in range(1, args.commits + 1):
        out.append(b"commit refs/heads/main\nmark :%d\n" % number)
        out.append(b"committer Big Repo <big@example.invalid> %d +0000\n" % (start + number * 60))
        data(f"commit {number}")
        if number == 1:
            for index in range(args.files):
                out.append(f"M 100644 inline {path(index)}\n".encode())
                data(content(index, 0))
        elif number == wide_at and args.wide:
            step = max(1, args.files // args.wide)
            for index in range(0, step * args.wide, step):
                out.append(f"M 100644 inline {path(index % args.files)}\n".encode())
                data(content(index % args.files, number))
        else:
            index = (number * 7919) % args.files
            out.append(f"M 100644 inline {path(index)}\n".encode())
            data(content(index, number))
        out.append(b"\n")
        if len(out) > 20_000:
            flush()
    for branch in range(args.branches):
        mark = 1 + (branch * (args.commits - 1)) // max(1, args.branches)
        out.append(f"reset refs/heads/topic/b{branch:03d}\nfrom :{mark}\n\n".encode())
    flush()
    stream.stdin.close()
    assert stream.wait() == 0, "git fast-import failed"

    git("checkout", "-q", "-f", "main")
    # As `git gc` leaves a repository: walks read the commit graph.
    git("commit-graph", "write", "--reachable")
    for stash in range(args.stashes):
        with open(os.path.join(args.dir, path(stash)), "a") as file:
            file.write(f"stash {stash}\n")
        git("stash", "push", "-q", "-m", f"stash {stash}")
    step = max(1, args.files // max(1, args.changes))
    for index in range(0, step * args.changes, step):
        with open(os.path.join(args.dir, path(index % args.files)), "a") as file:
            file.write("worktree edit\n")
    os.makedirs(os.path.join(args.dir, "new"), exist_ok=True)
    for index in range(args.untracked):
        with open(os.path.join(args.dir, "new", f"u{index:04d}.txt"), "w") as file:
            file.write(f"untracked {index}\n" * 5)


if __name__ == "__main__":
    main()
