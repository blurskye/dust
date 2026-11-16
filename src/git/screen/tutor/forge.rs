//! The forge lessons' practice forge: the fake forge of the integration tests
//! (`tests/fixtures/forge/server.py`, built into the binary) runs on loopback inside the
//! lesson directory, one server per lesson, with a small made-up world.
//!
//! The screen reaches it only through `crate::forge::sandbox`: while a forge lesson runs,
//! every forge setting comes from the sandbox (this server's address, a practice token or
//! none), never from the user's environment, config, CLI sign-ins or cache. The practice
//! repository's `forge` remote names a public host so detection works as it does for real,
//! and an unreachable proxy keeps Git itself from ever fetching from it.
use super::text;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The only token the practice forge accepts. It is a made-up value, not a credential.
pub(super) const TOKEN: &str = "ghp_fixture0123456789abcdefABCDEF0123";
/// The host the practice repository's remote names; requests go to loopback instead.
const HOST: &str = "github.com";
/// The practice repository's remote for the forge (`origin` stays the local bare repo).
pub(super) const REMOTE: &str = "forge";

const FILES: [(&str, &str); 5] = [
    (
        "server.py",
        include_str!("../../../../tests/fixtures/forge/server.py"),
    ),
    (
        "world_conversation.py",
        include_str!("../../../../tests/fixtures/forge/world_conversation.py"),
    ),
    (
        "world_gitlab.py",
        include_str!("../../../../tests/fixtures/forge/world_gitlab.py"),
    ),
    (
        "world_gitea.py",
        include_str!("../../../../tests/fixtures/forge/world_gitea.py"),
    ),
    (
        "world_actions.py",
        include_str!("../../../../tests/fixtures/forge/world_actions.py"),
    ),
];

/// Runs the server and exits when its stdin closes, so a server whose app died goes too.
const RUNNER: &str = r#"import os, runpy, sys, threading
def watch():
    try:
        sys.stdin.buffer.read()
    finally:
        os._exit(0)
threading.Thread(target=watch, daemon=True).start()
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum World {
    /// The `orbit-dev/orbit` project: pull requests, issues, notifications.
    Orbit,
    /// The same project without a sign-in: anonymous reads only.
    Anonymous,
    /// `octo/orbit`'s Actions: ten runs, one failing its tests.
    Actions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Token {
    /// The practice token is set from the start.
    Signed,
    /// No token: the screen browses anonymously.
    Missing,
    /// No token at first; the lesson adds one when it starts.
    Added,
}

/// A forge lesson: its world, its sign-in and the tab (and item) the screen opens on.
pub(super) struct Forge {
    pub world: World,
    pub token: Token,
    /// `git`, `prs`, `issues`, `actions` or `notifications`.
    pub page: &'static str,
    pub item: Option<u64>,
    /// A branch of the practice repository's own, one commit ahead of `main`, checked out.
    pub branch: Option<&'static str>,
}

impl Forge {
    pub(super) const fn new(world: World, token: Token, page: &'static str) -> Self {
        Self {
            world,
            token,
            page,
            item: None,
            branch: None,
        }
    }
    pub(super) const fn item(mut self, item: u64) -> Self {
        self.item = Some(item);
        self
    }
    pub(super) const fn branch(mut self, branch: &'static str) -> Self {
        self.branch = Some(branch);
        self
    }
    /// The remote's URL for this world's repository.
    pub(super) fn remote_url(&self) -> String {
        match self.world {
            World::Actions => format!("ssh://git@{HOST}/octo/orbit.git"),
            World::Orbit | World::Anonymous => format!("ssh://git@{HOST}/orbit-dev/orbit.git"),
        }
    }
}

/// One lesson's fake forge process.
pub(super) struct Server {
    child: Child,
    port: u16,
    dir: PathBuf,
    state: PathBuf,
}

impl Server {
    pub(super) fn start(lesson: &Path, world: World) -> Result<Self, String> {
        let dir = lesson.join("forge");
        std::fs::create_dir_all(&dir).map_err(text)?;
        for (name, source) in FILES {
            std::fs::write(dir.join(name), source).map_err(text)?;
        }
        std::fs::write(dir.join("run.py"), RUNNER).map_err(text)?;
        let state = dir.join("state.json");
        std::fs::write(&state, self::world(world).to_string()).map_err(text)?;
        // Python's own words when the server stops before it listens.
        let log = std::fs::File::create(dir.join("server.log")).map_err(text)?;
        let mut missing = String::new();
        for python in ["python3", "python"] {
            let mut command = Command::new(python);
            command
                .arg("-I")
                .arg(dir.join("run.py"))
                .arg(dir.join("server.py"))
                .arg("--state")
                .arg(&state)
                .current_dir(&dir)
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(log.try_clone().map_err(text)?);
            for name in ["PATH", "SYSTEMROOT", "TEMP", "TMP", "LANG"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            match command.spawn() {
                Ok(child) => return Self::ready(child, dir, state),
                Err(error) => missing = error.to_string(),
            }
        }
        Err(format!(
            "The practice forge needs Python 3 on PATH ({missing})"
        ))
    }

    /// Waits for the port the server prints once it listens.
    fn ready(mut child: Child, dir: PathBuf, state: PathBuf) -> Result<Self, String> {
        let stdout = child
            .stdout
            .take()
            .ok_or("The practice forge has no output")?;
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = send.send(line);
        });
        let port = receive
            .recv_timeout(Duration::from_secs(30))
            .ok()
            .and_then(|line| line.trim().parse::<u16>().ok());
        match port {
            Some(port) => Ok(Self {
                child,
                port,
                dir,
                state,
            }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let log = std::fs::read_to_string(dir.join("server.log")).unwrap_or_default();
                Err(
                    match log.lines().rev().find(|line| !line.trim().is_empty()) {
                        Some(line) => format!("The practice forge did not start: {}", line.trim()),
                        None => "The practice forge did not start".into(),
                    },
                )
            }
        }
    }

    /// The sandbox the screen runs in for this lesson. Only this server's address and,
    /// when signed in, the practice token: no `HOME`, so no CLI sign-in store either.
    pub(super) fn sandbox(&self, signed_in: bool) -> BTreeMap<String, String> {
        let url = format!("http://127.0.0.1:{}", self.port);
        let mut vars = BTreeMap::from([
            (
                "DUSTY_FORGE_API_BASE".to_owned(),
                format!("{url}/{{host}}/api/v3"),
            ),
            ("DUSTY_FORGE_ORIGIN".to_owned(), format!("{url}/{{host}}")),
            (
                "DUSTY_FORGE_STATE_DIR".to_owned(),
                self.dir.join("state").to_string_lossy().into_owned(),
            ),
            ("DUSTY_FORGE_HOST".to_owned(), HOST.to_owned()),
            ("DUSTY_FORGE_REMOTE".to_owned(), REMOTE.to_owned()),
        ]);
        if signed_in {
            vars.insert("DUSTY_FORGE_TOKEN".into(), TOKEN.into());
        }
        vars
    }

    /// Every request the practice forge answered, oldest first.
    pub(super) fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.state)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|mut state| state.get_mut("requests").map(Value::take))
            .and_then(|requests| match requests {
                Value::Array(requests) => Some(requests),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Whether the practice forge answered a write `request(method, path, body)` passes.
    /// Paths are the API's own (`repos/OWNER/NAME/pulls/13/merge`).
    pub(super) fn wrote(&self, request: impl Fn(&str, &str, &Value) -> bool) -> bool {
        self.requests().iter().any(|entry| {
            let method = entry["method"].as_str().unwrap_or("");
            method != "GET" && request(method, entry["path"].as_str().unwrap_or(""), &entry["body"])
        })
    }
}

/// Keeps the sandbox for as long as a forge-track session lives (`:tutor quit`, or the
/// screen closing with the tutor open).
pub(super) struct Sandbox(pub bool);

impl Drop for Sandbox {
    fn drop(&mut self) {
        if self.0 {
            crate::forge::sandbox::leave();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The pull requests whose head the practice forge's Git serves (`refs/pull/N/head`).
const PULLS: [(u64, &str); 5] = [
    (13, "fix/backoff"),
    (14, "feat/14"),
    (16, "feat/16"),
    (17, "feat/17"),
    (18, "feat/18"),
];

/// The practice repository's forge remote. Git itself never reaches the host: the remote
/// is reached over `ssh`, and the repository's `ssh` is a script that serves the lesson's own
/// bare repository (`origin`), which holds each practice pull request's head; any HTTP
/// request of this repository goes to a closed loopback port.
pub(super) fn configure_repository(
    git: impl Fn(&[&str]) -> Result<String, String>,
    forge: &Forge,
) -> Result<(), String> {
    git(&["remote", "add", REMOTE, &forge.remote_url()])?;
    git(&["config", "http.proxy", "http://127.0.0.1:9"])?;
    git(&["config", "credential.helper", ""])?;
    let bare = git(&["remote", "get-url", "origin"])?.trim().to_owned();
    let dir = PathBuf::from(git(&["rev-parse", "--absolute-git-dir"])?.trim());
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let script = dir.join("practice-forge-ssh.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n# The practice forge's Git: serves this lesson's bare repository, never a host.\n\
             case \"$2\" in\n  git-receive-pack*) exec git receive-pack {bare} ;;\n  *) exec git upload-pack {bare} ;;\nesac\n",
            bare = quote(&bare)
        ),
    )
    .map_err(text)?;
    let script = script.to_string_lossy().replace('\\', "/");
    git(&[
        "config",
        "core.sshCommand",
        &format!("sh {}", quote(&script)),
    ])?;
    git(&["config", "ssh.variant", "simple"])?;
    if forge.world != World::Actions {
        let mut push = vec!["push".to_owned(), "-q".into(), "origin".into()];
        for (number, branch) in PULLS {
            push.push(format!("HEAD:refs/pull/{number}/head"));
            push.push(format!("HEAD:refs/heads/{branch}"));
        }
        git(&push.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    if let Some(branch) = forge.branch {
        git(&["switch", "-q", "-c", branch])?;
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("Practice change on {branch}"),
        ])?;
    }
    Ok(())
}

// --------------------------------------------------------------------------- worlds

fn world(world: World) -> Value {
    match world {
        World::Actions => {
            json!({ "world": "actions", "actions_seed": "design", "actions_waiting": true, "viewer": "maya" })
        }
        World::Orbit => orbit(),
        World::Anonymous => {
            let mut world = orbit();
            world["anonymous"] = json!({ "limit": 60, "remaining": 57 });
            world
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn ago(seconds: u64) -> String {
    crate::forge::incremental::rfc3339(now().saturating_sub(seconds)).0
}

const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

fn person(login: &str) -> Value {
    json!({ "login": login })
}

fn label(name: &str) -> Value {
    let color = match name {
        "bug" => "d73a4a",
        "enhancement" => "a2eeef",
        "design" => "c5a3f2",
        "accessibility" => "7aa2f7",
        "documentation" => "0075ca",
        _ => "e4c46a",
    };
    json!({ "name": name, "color": color })
}

/// The fake forge merges each entry over its own default item; clear what orbit lacks.
fn cleared(mut entry: Value) -> Value {
    for (key, value) in [
        ("assignees", json!([])),
        ("milestone", Value::Null),
        ("projectItems", json!([])),
        ("closedByPullRequestsReferences", json!([])),
        ("comments", json!([])),
    ] {
        if entry.get(key).is_none() {
            entry[key] = value;
        }
    }
    entry
}

fn issue(
    number: u64,
    title: &str,
    author: &str,
    labels: &[&str],
    updated: u64,
    body: &str,
) -> Value {
    cleared(json!({
        "number": number,
        "title": title,
        "author": person(author),
        "labels": labels.iter().map(|name| label(name)).collect::<Vec<_>>(),
        "updatedAt": ago(updated),
        "createdAt": ago(updated + DAY),
        "body": body,
    }))
}

fn check(name: &str, conclusion: Option<&str>, run: u64) -> Value {
    json!({
        "__typename": "CheckRun",
        "name": name,
        "conclusion": conclusion,
        "status": if conclusion.is_some() { "COMPLETED" } else { "IN_PROGRESS" },
        "startedAt": ago(9 * MINUTE),
        "completedAt": ago(9 * MINUTE - 3),
        "detailsUrl": format!("https://{HOST}/orbit-dev/orbit/actions/runs/{run}/job/{}", run * 10),
        "workflowName": "Quality checks",
    })
}

fn passed() -> Value {
    json!([
        check("Python tests", Some("SUCCESS"), 6),
        check("Lint", Some("SUCCESS"), 6)
    ])
}

struct Pull<'a> {
    number: u64,
    title: &'a str,
    author: &'a str,
    decision: &'a str,
    checks: Value,
    updated: u64,
}

fn pull(pull: Pull, extra: Value) -> Value {
    let Pull {
        number,
        title,
        author,
        decision,
        checks,
        updated,
    } = pull;
    let mut entry = cleared(json!({
        "closingIssuesReferences": [],
        "reviews": [],
        "reviewRequests": [],
        "commits": [{
            "oid": format!("{number:02}c91d2{}", "0".repeat(33)),
            "messageHeadline": title,
            "messageBody": "",
            "committedDate": ago(updated),
            "authors": [{ "login": author }],
        }],
        "mergeable": "MERGEABLE",
        "headRefOid": format!("{number:02}{}", "a".repeat(38)),
        "number": number,
        "title": title,
        "author": person(author),
        "reviewDecision": decision,
        "statusCheckRollup": checks,
        "additions": 6,
        "deletions": 2,
        "changedFiles": 2,
        "updatedAt": ago(updated),
        "createdAt": ago(updated),
        "labels": [],
        "isDraft": false,
        "headRefName": format!("feat/{number}"),
        "baseRefName": "main",
        "mergeStateStatus": "BLOCKED",
        "files": [
            { "path": "orbit/queue.py", "additions": 4, "deletions": 1 },
            { "path": "tests/test_queue.py", "additions": 2, "deletions": 1 },
        ],
        "body": format!("{title}."),
    }));
    if let (Value::Object(entry), Value::Object(extra)) = (&mut entry, extra) {
        entry.extend(extra);
    }
    entry
}

const DIFF: &str = "diff --git a/orbit/queue.py b/orbit/queue.py
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
";

fn notification(
    id: &str,
    reason: &str,
    kind: &str,
    number: Option<u64>,
    title: &str,
    unread: bool,
    age: u64,
) -> Value {
    let path = match kind {
        "PullRequest" => Some("pulls"),
        "Issue" => Some("issues"),
        _ => None,
    };
    let url = path.zip(number).map(|(path, number)| {
        format!("https://api.github.com/repos/orbit-dev/orbit/{path}/{number}")
    });
    json!({
        "id": id,
        "unread": unread,
        "reason": reason,
        "updated_at": ago(age),
        "repository": { "full_name": "orbit-dev/orbit" },
        "subject": { "title": title, "type": kind, "url": url, "latest_comment_url": null },
    })
}

/// A small `orbit-dev/orbit`: the signed-in user is maya.
fn orbit() -> Value {
    let issues = vec![
        issue(
            20,
            "Support reduced-motion terminal themes",
            "maya",
            &["design", "accessibility"],
            20,
            "Some terminals render the spinner as flicker.\n\nOffer a **reduced motion** theme flag.",
        ),
        issue(
            19,
            "Preserve scroll position in long logs",
            "alex",
            &["bug"],
            18 * MINUTE,
            "Scrolling a long job log jumps back to the top when new lines arrive.",
        ),
        issue(
            18,
            "Add keyboard shortcuts to issue filters",
            "sam",
            &["enhancement"],
            HOUR,
            "`[` and `]` should move between saved sections.",
        ),
        issue(
            17,
            "Improve empty repository guidance",
            "nina",
            &["design"],
            2 * HOUR,
            "Say what to do next.",
        ),
        issue(
            7,
            "Reconnect loses queued progress",
            "nina",
            &["bug"],
            2 * DAY,
            "Progress counters reset after a reconnect.",
        ),
        issue(
            1,
            "Queue stalls after a worker reconnects",
            "alex",
            &["bug", "priority: high"],
            38 * MINUTE,
            "When a worker reconnects, queued tasks can stay pending until another task arrives.",
        ),
    ];
    let prs = vec![
        pull(
            Pull {
                number: 13,
                title: "Clamp exponential backoff to sixty seconds",
                author: "maya",
                decision: "REVIEW_REQUIRED",
                checks: passed(),
                updated: 9 * MINUTE,
            },
            json!({ "labels": [label("priority: high")], "headRefName": "fix/backoff",
                    "reviewRequests": ["alex", "sam"],
                    "body": "Cap retry delays at sixty seconds, even after a long outage.",
                    "closingIssuesReferences": [{ "number": 1, "title": "Queue stalls after a worker reconnects" }] }),
        ),
        pull(
            Pull {
                number: 17,
                title: "Export completed task history as JSON",
                author: "leo",
                decision: "REVIEW_REQUIRED",
                checks: passed(),
                updated: 40 * MINUTE,
            },
            json!({ "reviewRequests": ["maya"] }),
        ),
        pull(
            Pull {
                number: 14,
                title: "Introduce a quiet status indicator",
                author: "nina",
                decision: "APPROVED",
                checks: passed(),
                updated: 2 * HOUR,
            },
            json!({ "mergeStateStatus": "CLEAN",
                    "reviews": [{ "author": person("maya"), "state": "APPROVED", "body": "Looks good." }] }),
        ),
        pull(
            Pull {
                number: 18,
                title: "Exercise the retry-limit regression check",
                author: "leo",
                decision: "REVIEW_REQUIRED",
                checks: json!([
                    check("Python tests", Some("FAILURE"), 7),
                    check("Lint", Some("SUCCESS"), 7)
                ]),
                updated: DAY,
            },
            json!({ "reviewRequests": ["maya"] }),
        ),
        pull(
            Pull {
                number: 16,
                title: "Batch terminal progress notifications",
                author: "sam",
                decision: "",
                checks: json!([check("Python tests", None, 8)]),
                updated: 2 * DAY,
            },
            json!({ "isDraft": true }),
        ),
    ];
    let mut timelines = serde_json::Map::new();
    for entry in &issues {
        let number = entry["number"].as_u64().unwrap_or_default().to_string();
        timelines.insert(number, json!([]));
    }
    timelines.insert(
        "13".into(),
        json!([{ "event": "commented", "user": person("alex"), "created_at": ago(HOUR), "node_id": "C13",
                 "body": "Does this keep **negative** attempts at one second?" }]),
    );
    let threads = prs
        .iter()
        .map(|entry| (format!("orbit-dev/orbit#{}", entry["number"]), json!([])))
        .collect::<serde_json::Map<_, _>>();
    let log = [
        "tests/test_retry.py::test_backoff_caps_at_sixty PASSED",
        "tests/test_retry.py::test_retry_limit_stops_after_five FAILED",
        "E   AssertionError: expected 5 attempts, saw 6",
        "=========== 1 failed, 41 passed in 2.71s ===========",
    ]
    .iter()
    .map(|line| format!("Python tests\tRun pytest\t{} {line}\n", ago(DAY)))
    .collect::<String>();
    let step = |number: u64, name: &str, conclusion: &str| {
        json!({ "name": name, "number": number, "status": "completed", "conclusion": conclusion,
                "startedAt": ago(DAY), "completedAt": ago(DAY - 1) })
    };
    let run = json!({
        "databaseId": 7, "number": 214, "displayTitle": "Exercise the retry-limit regression check",
        "workflowName": "Quality checks", "workflowDatabaseId": 3, "headBranch": "feat/18",
        "headSha": "b".repeat(40), "event": "pull_request", "attempt": 1, "status": "completed",
        "conclusion": "failure", "path": ".github/workflows/quality.yml", "createdAt": ago(DAY),
        "jobs": [{ "databaseId": 70, "name": "Python tests", "status": "completed", "conclusion": "failure",
                   "startedAt": ago(DAY), "completedAt": ago(DAY - 3),
                   "steps": [step(1, "Set up job", "success"), step(2, "Run pytest", "failure")] }],
    });
    json!({
        "repository": "orbit-dev/orbit",
        "viewer": "maya",
        // Pull request 13 and issue 1 carry a full conversation (world_conversation.py).
        "conversation": true,
        "filter_terms": true,
        "issues": issues,
        "prs": prs,
        "timelines": timelines,
        "review_threads": threads,
        "diffs": { "13": DIFF, "17": DIFF, "14": DIFF, "18": DIFF, "16": DIFF },
        "diff": DIFF,
        "comments": [],
        "posted_at": ago(5),
        "run": run,
        "run_views": 2,
        "logs": { "70": log },
        "notifications": [
            notification("8101", "review_requested", "PullRequest", Some(13),
                         "Clamp exponential backoff to sixty seconds", true, 2 * MINUTE),
            notification("8102", "mention", "Issue", Some(7), "Reconnect loses queued progress", true, 14 * MINUTE),
            notification("8104", "review_requested", "PullRequest", Some(17),
                         "Export completed task history as JSON", true, 2 * HOUR),
            notification("8106", "author", "Issue", Some(1), "Queue stalls after a worker reconnects", false, DAY),
        ],
    })
}
