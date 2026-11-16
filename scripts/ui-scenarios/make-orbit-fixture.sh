#!/usr/bin/env bash
# Build the `orbit` fixture repository every Dusty Git mockup and screenshot uses
#
#
#   scripts/ui-scenarios/make-orbit-fixture.sh DIR
#
# DIR is replaced. Commits use fixed authors and dates so hashes are reproducible
# for a given Git version. The working tree ends with seven changes:
# two fully staged, one partly staged (MM), three unstaged, one untracked.
set -euo pipefail
dir=${1:?usage: make-orbit-fixture.sh DIR}
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q -b main
git config user.name "Alex Kim"
git config user.email ak@orbit.dev
git config commit.gpgsign false
git config tag.gpgsign false
git remote add origin https://github.com/orbit-dev/orbit.git

commit() { # author email date message
  GIT_AUTHOR_NAME=$1 GIT_AUTHOR_EMAIL=$2 GIT_AUTHOR_DATE=$3 \
  GIT_COMMITTER_NAME=$1 GIT_COMMITTER_EMAIL=$2 GIT_COMMITTER_DATE=$3 \
    git commit -q -m "$4"
}
SR=("Sam Rao" sr@orbit.dev)
LM=("Lea Moss" lm@orbit.dev)
AK=("Alex Kim" ak@orbit.dev)

mkdir -p orbit tests docs
cat > pyproject.toml <<'EOF'
[project]
name = "orbit"
version = "0.4.0"
requires-python = ">=3.11"
EOF
cat > orbit/__init__.py <<'EOF'
"""Orbit: a small durable task queue."""
EOF
cat > orbit/queue.py <<'EOF'
"""The task queue."""

from orbit.config import RETRY_BASE


class Queue:
    def __init__(self):
        self._items = []


def retry_delay(attempt: int) -> int:
    """Return retry delay in seconds."""

    return min(2 ** max(0, attempt), 60)


RETRY_LIMIT = 8


class Tasks(Queue):
    def push(self, task):
        self._items.append(task)

    def queue_size(self) -> int:
        """Return size."""
        return len(self._items)
EOF
cat > orbit/worker.py <<'EOF'
"""Workers pull tasks from the queue."""


class Worker:
    def __init__(self, queue):
        self.queue = queue

    def notify(self, events):
        for event in events:
            self.queue.push(event)
EOF
cat > orbit/config.py <<'EOF'
"""Configuration defaults."""

RETRY_BASE = 2
EOF
cat > tests/test_queue.py <<'EOF'
from orbit.queue import retry_delay


def test_retry_delay_grows():
    assert retry_delay(1) == 2


def test_retry_delay_saturates():
    assert retry_delay(6) == 60
    assert retry_delay(-1) == 1
EOF
cat > docs/retries.md <<'EOF'
# Retries

Failed tasks are retried with exponential backoff.
EOF
git add -A
commit "${SR[@]}" "2026-11-10T09:12:00" "Tag queue release"
git branch release/0.4
git tag -a v0.4.0 -m "Orbit 0.4.0"
git tag v0.3.2 HEAD

printf '\n    def notify_batch(self, events):\n        self.notify(list(events))\n' >> orbit/worker.py
git add -A
commit "${LM[@]}" "2026-11-10T15:40:00" "Batch worker notifications"

printf '\n\ndef recover(queue, pending):\n    for task in pending:\n        queue.push(task)\n' >> orbit/queue.py
git add -A
commit "${SR[@]}" "2026-11-11T11:05:00" "Recover queued tasks"
git update-ref refs/remotes/origin/main HEAD

git switch -q -c feat/search
cat > orbit/search.py <<'EOF'
def search_tasks(tasks, query):
    needle = query.casefold()
    return [task for task in tasks if needle in task.title.casefold()]
EOF
git add -A
commit "${LM[@]}" "2026-11-11T16:20:00" "Match task history"
git update-ref refs/remotes/origin/feat/search HEAD

git switch -q main
git switch -q -c fix/backoff
cat >> tests/test_queue.py <<'EOF'


def test_retry_delay_is_capped():
    assert retry_delay(20) == 60
EOF
git add -A
commit "${AK[@]}" "2026-11-12T09:30:00" "Add backoff regression test"
git update-ref refs/remotes/origin/fix/backoff HEAD~0
python3 - <<'EOF'
import pathlib
p = pathlib.Path("orbit/queue.py")
p.write_text(p.read_text().replace("RETRY_LIMIT = 8", "RETRY_LIMIT = 8  # attempts"))
EOF
git add -A
commit "${AK[@]}" "2026-11-12T10:42:00" "Bound retry exponent"
# origin/fix/backoff stays two commits behind HEAD: ↑2 ↓0.
git update-ref refs/remotes/origin/fix/backoff HEAD~2
git branch -q --set-upstream-to=origin/fix/backoff fix/backoff 2>/dev/null || \
  git config branch.fix/backoff.remote origin
git config branch.fix/backoff.merge refs/heads/fix/backoff
git config branch.main.remote origin
git config branch.main.merge refs/heads/main
git config branch.feat/search.remote origin
git config branch.feat/search.merge refs/heads/feat/search

# A stash on another branch name.
printf '\n    def reconnect(self):\n        self.close()\n' >> orbit/worker.py
git stash push -q -m "WIP reconnect"

# Working tree: the seven changes.
printf 'license = "MIT"\n' >> pyproject.toml
python3 - <<'EOF'
import pathlib
p = pathlib.Path("docs/retries.md")
p.write_text(p.read_text().replace(
    "Failed tasks are retried with exponential backoff.",
    "Failed tasks are retried with exponential backoff.\n"
    "The delay is capped at sixty seconds.\n"
    "Negative attempts wait one second.\n"))
EOF
# queue.py: the docstring edit is staged, the retry rewrite stays in the worktree (MM).
python3 - <<'EOF'
import pathlib
p = pathlib.Path("orbit/queue.py")
p.write_text(p.read_text().replace('"""Return size."""', '"""Return the number of queued tasks."""'))
EOF
git add pyproject.toml docs/retries.md orbit/queue.py
python3 - <<'EOF'
import pathlib
p = pathlib.Path("orbit/queue.py")
p.write_text(p.read_text().replace(
    "    return min(2 ** max(0, attempt), 60)\n",
    "    # Cap the exponent before computing the delay.\n"
    "    bounded_attempt = min(max(0, attempt), 6)\n"
    "    return min(1 << bounded_attempt, 60)\n"))
EOF
python3 - <<'EOF'
import pathlib
p = pathlib.Path("tests/test_queue.py")
p.write_text(p.read_text().replace("assert retry_delay(6) == 60", "assert retry_delay(10_000) == 60"))
p = pathlib.Path("orbit/worker.py")
p.write_text(p.read_text().replace(
    "        for event in events:\n",
    "        # Deliver in arrival order.\n        for event in events:\n"
    ).replace("self.queue.push(event)", "self.queue.push(event)  # one at a time"))
p = pathlib.Path("orbit/config.py")
p.write_text(p.read_text() + "RETRY_CAP = 60\nRETRY_MAX_EXPONENT = 6\n")
EOF
cat > docs/backoff.md <<'EOF'
# Backoff

Retry delays double with every attempt:

| attempt | delay |
|---|---|
| 1 | 2s |
| 6 | 60s (cap) |
EOF
git status --short
