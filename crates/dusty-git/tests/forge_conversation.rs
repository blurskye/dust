#![cfg(unix)]
//! The fake forge's real-looking conversation (`state["conversation"]`,
//! `tests/fixtures/forge/world_conversation.py`) read through `dusty-git forge` on every
//! provider: the pull request's description, commits, events, people and bot comments, its
//! review threads with their diff hunks, and the issue it closes with its own discussion.

#[path = "../../../tests/support/forge_fake.rs"]
mod forge_fake;
#[path = "../../../tests/support/forge_world.rs"]
mod forge_world;

use forge_fake::FAKE_TOKEN;
use forge_world::ALL;
use forge_world::Forge;
use forge_world::World;
use serde_json::Value;
use serde_json::json;
use std::process::Command;
use std::process::Stdio;

/// The conversation's pull (merge) request and issue in each world.
fn numbers(forge: Forge) -> (u64, u64) {
    match forge {
        Forge::GitHub => (13, 1),
        Forge::GitLab => (15, 16),
        Forge::Gitea => (1412, 1413),
        Forge::Forgejo => (1794, 1795),
    }
}

/// `data` of one `dusty-git forge` read.
fn read(world: &World, args: &[&str]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dusty-git"));
    command
        .arg("forge")
        .arg("--repo")
        .arg(&world.repo)
        .args(args)
        .current_dir(&world.repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    world.configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(FAKE_TOKEN) && !stderr.contains(FAKE_TOKEN),
        "the token leaked"
    );
    assert!(
        output.status.success(),
        "{args:?}: exit {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    let object: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("{args:?}: not one JSON object ({error}): {stdout}"));
    object["data"].clone()
}

/// A list read's rows: one JSON object per line.
fn read_lines(world: &World, args: &[&str]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dusty-git"));
    command
        .arg("forge")
        .arg("--repo")
        .arg(&world.repo)
        .args(args)
        .current_dir(&world.repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    world.configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(FAKE_TOKEN) && !stderr.contains(FAKE_TOKEN),
        "the token leaked"
    );
    assert!(output.status.success(), "{args:?}: {stdout}\n{stderr}");
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{args:?}: {error}: {line}"))
        })
        .collect()
}

/// The timeline's `(kind, fields)` pairs (serde's external tagging).
fn events(timeline: &Value) -> Vec<(String, Value)> {
    timeline
        .as_array()
        .unwrap_or_else(|| panic!("not a timeline: {timeline}"))
        .iter()
        .map(|event| {
            let (kind, fields) = event
                .as_object()
                .and_then(|object| object.iter().next())
                .unwrap_or_else(|| panic!("not a tagged event: {event}"));
            (kind.clone(), fields.clone())
        })
        .collect()
}

fn comments(events: &[(String, Value)]) -> Vec<&Value> {
    events
        .iter()
        .filter(|(kind, _)| kind == "Commented")
        .map(|(_, comment)| comment)
        .collect()
}

fn has(events: &[(String, Value)], kind: &str) -> bool {
    events.iter().any(|(each, _)| each == kind)
}

fn bot_comment(comments: &[&Value]) -> bool {
    comments
        .iter()
        .any(|comment| comment["author"]["bot"] == true)
}

/// The reactions on an item's own body, as `(content, count)`.
fn body_reactions(detail: &Value) -> Vec<(String, u64)> {
    detail["reactions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|each| {
            (
                each["content"].as_str().unwrap_or_default().to_owned(),
                each["count"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

fn check_pull(world: &World, forge: Forge, pull: u64, issue: u64) {
    let number = pull.to_string();
    let detail = read(world, &["pr", &number]);
    if forge != Forge::GitHub {
        // Gitea's `issues/{n}/reactions`, GitLab's `award_emoji`: GitHub's words, GitHub's order.
        assert_eq!(
            body_reactions(&detail),
            [("+1".to_owned(), 2), ("rocket".to_owned(), 1)],
            "the description's reactions: {}",
            detail["reactions"]
        );
    }
    let body = detail["body"].as_str().unwrap_or_default();
    for part in [
        "## Summary",
        "- Bound the exponent",
        "```python",
        "https://orbit.example/docs/retries",
    ] {
        assert!(body.contains(part), "description lacks {part:?}: {body}");
    }
    assert!(body.contains(&format!("Closes #{issue}")), "{body}");
    let labels = detail["row"]["labels"].as_array().map_or(0, Vec::len);
    assert_eq!(labels, 2, "labels: {}", detail["row"]["labels"]);
    assert_eq!(
        detail["assignees"][0]["login"], "maya",
        "{}",
        detail["assignees"]
    );
    assert_eq!(
        detail["milestone"]["title"], "v1.4",
        "{}",
        detail["milestone"]
    );
    assert_eq!(
        detail["commits"].as_array().map_or(0, Vec::len),
        3,
        "commits: {}",
        detail["commits"]
    );
    // What it closes: the forge's own list, else (Gitea, Forgejo) the description's
    // `Closes #N`, with the issue's title.
    let linked = detail["linked_issues"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        linked.iter().any(|link| link["number"] == issue
            && link["title"] == "Queue stalls after a worker reconnects"),
        "linked issues: {}",
        detail["linked_issues"]
    );
    check_ci(world, forge, &detail);

    let timeline = read(world, &["timeline", &number]);
    let events = events(&timeline);
    let said = comments(&events);
    assert!(bot_comment(&said), "no bot comment: {timeline}");
    assert!(
        said.iter()
            .filter(|comment| comment["author"]["bot"] == true)
            .all(|comment| {
                let text = comment["body"].as_str().unwrap_or_default();
                let table = text
                    .lines()
                    .any(|line| line.starts_with('|') && line.contains("---"));
                text.contains("<details><summary>Walkthrough</summary>") && table
            }),
        "bot comments carry a walkthrough and a table: {timeline}"
    );
    assert!(
        said.iter()
            .filter(|comment| comment["author"]["bot"] == true)
            .count()
            >= 2,
        "two bots: {timeline}"
    );
    let people = said
        .iter()
        .filter(|comment| comment["author"]["bot"] == false)
        .map(|comment| comment["author"]["login"].as_str().unwrap_or_default())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        people.len() >= 4,
        "comments from several people: {people:?}"
    );
    assert!(
        said.iter()
            .any(|comment| comment["body"].as_str().unwrap_or_default().lines().count() >= 40),
        "a long comment: {timeline}"
    );
    assert!(
        said.iter().any(|comment| comment["edited"].is_string()),
        "an edited comment: {timeline}"
    );
    if forge == Forge::GitHub {
        assert!(
            said.iter()
                .any(|comment| comment["association"] == "CONTRIBUTOR"),
            "author association: {timeline}"
        );
        let reacted = said
            .iter()
            .filter(|comment| {
                comment["reactions"]
                    .as_array()
                    .is_some_and(|all| !all.is_empty())
            })
            .count();
        assert!(
            reacted >= 2,
            "reactions on a couple of comments: {timeline}"
        );
    }
    for kind in ["Labeled", "Assigned", "Milestoned", "ReviewRequested"] {
        assert!(has(&events, kind), "no {kind}: {timeline}");
    }
    let verdicts = events
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| kind == "Reviewed")
        .map(|(index, (_, review))| {
            (
                index,
                review["state"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let changes = verdicts
        .iter()
        .find(|(_, state)| state == "ChangesRequested")
        .unwrap_or_else(|| panic!("no change request: {timeline}"))
        .0;
    let approved = verdicts
        .iter()
        .rfind(|(_, state)| state == "Approved")
        .unwrap_or_else(|| panic!("no approval: {timeline}"))
        .0;
    assert!(
        changes < approved,
        "approved after the change request: {verdicts:?}"
    );
    assert!(
        events[changes..approved].iter().any(|(kind, fields)| {
            kind == "Committed"
                || kind == "ForcePushed"
                || (kind == "Other"
                    && fields["kind"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("pushed"))
        }),
        "a push between the change request and the approval: {timeline}"
    );
    if forge != Forge::GitLab {
        // GitLab keeps no force-push event.
        assert!(has(&events, "ForcePushed"), "a force-push: {timeline}");
    }

    let threads = read(world, &["threads", &number]);
    let threads = threads
        .as_array()
        .unwrap_or_else(|| panic!("threads: {threads}"));
    let resolved = threads
        .iter()
        .find(|thread| thread["resolved"] == true)
        .unwrap_or_else(|| panic!("no resolved thread: {threads:?}"));
    assert_eq!(resolved["path"], "orbit/queue.py");
    assert_eq!(resolved["line"], 14);
    assert_eq!(
        resolved["comments"].as_array().map_or(0, Vec::len),
        3,
        "{resolved}"
    );
    let open = threads
        .iter()
        .find(|thread| thread["resolved"] == false)
        .unwrap_or_else(|| panic!("no open thread: {threads:?}"));
    assert_eq!(open["path"], "tests/test_queue.py");
    assert_eq!(open["comments"].as_array().map_or(0, Vec::len), 1, "{open}");
    if forge != Forge::GitLab {
        // GitLab's discussions carry a position, not the hunk.
        for thread in [resolved, open] {
            let hunk = thread["diff_hunk"].as_str().unwrap_or_default();
            assert!(hunk.starts_with("@@ "), "a diff hunk: {thread}");
        }
    }
}

/// The head's checks open their job and its log: Gitea's and Forgejo's status links a run
/// whose job log reads; GitLab's pipeline has a failed pytest job allowed to fail.
fn check_ci(world: &World, forge: Forge, detail: &Value) {
    let checks = detail["checks"].as_array().cloned().unwrap_or_default();
    match forge {
        Forge::Gitea | Forge::Forgejo => {
            let test = checks
                .iter()
                .find(|check| check["name"] == "ci / test (pull_request)")
                .unwrap_or_else(|| panic!("checks: {checks:?}"));
            let run = test["run_link"]["run"]
                .as_str()
                .unwrap_or_else(|| panic!("run link: {test}"));
            // The status links run index 6; the API reads that run by its id.
            assert!(
                test["details_url"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("/actions/runs/6/"),
                "{test}"
            );
            assert_eq!(run, "4407", "the run's index resolves to its id: {test}");
            let run = read(world, &["run", run]);
            let job = run["jobs"]
                .as_array()
                .and_then(|jobs| jobs.iter().find(|job| job["name"] == "test"))
                .unwrap_or_else(|| panic!("run jobs: {run}"));
            let id = job["id"]
                .as_str()
                .unwrap_or_else(|| panic!("job id: {job}"));
            let log = read(world, &["log", id]);
            assert!(
                log["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("44 passed"),
                "log: {log}"
            );
        }
        Forge::GitLab => {
            assert_eq!(checks.len(), 3, "three jobs: {checks:?}");
            let failed = checks
                .iter()
                .find(|check| check["state"] == "Failed")
                .unwrap_or_else(|| panic!("a failed job: {checks:?}"));
            let url = failed["details_url"]
                .as_str()
                .or_else(|| failed["url"].as_str())
                .unwrap_or_default();
            let job = url.rsplit('/').next().unwrap_or_default();
            let log = read(world, &["log", job]);
            let text = log["text"].as_str().unwrap_or_default();
            assert!(text.contains("1 failed, 43 passed"), "pytest log: {log}");
        }
        Forge::GitHub => {
            // The list row counts alex's approval against sam's open request.
            let rows = read_lines(world, &["prs"]);
            let row = rows
                .iter()
                .find(|row| row["number"] == detail["row"]["number"])
                .unwrap_or_else(|| panic!("the list holds the pull request: {rows:?}"));
            assert_eq!(
                row["review_tally"],
                json!({"approved": 1, "changes_requested": 0, "pending": 1}),
                "{row}"
            );
        }
    }
}

fn check_issue(world: &World, forge: Forge, pull: u64, issue: u64) {
    let number = issue.to_string();
    let detail = read(world, &["issue", &number]);
    assert_eq!(
        detail["row"]["labels"].as_array().map_or(0, Vec::len),
        2,
        "{detail}"
    );
    if forge != Forge::GitHub {
        assert_eq!(
            body_reactions(&detail),
            [("+1".to_owned(), 1), ("eyes".to_owned(), 1)],
            "the issue body's reactions: {}",
            detail["reactions"]
        );
    }
    assert_eq!(detail["assignees"][0]["login"], "maya", "{detail}");
    assert!(
        detail["body"]
            .as_str()
            .unwrap_or_default()
            .contains("### Steps to reproduce"),
        "{detail}"
    );
    if forge == Forge::GitLab {
        // `timeline N` names no kind and a new process remembers none: GitLab reads the merge
        // request's notes. The contract suite reads this issue's timeline in one process.
        return;
    }
    let timeline = read(world, &["timeline", &number]);
    let events = events(&timeline);
    let said = comments(&events);
    assert!(said.len() >= 5, "five comments or more: {timeline}");
    assert!(bot_comment(&said), "a bot comment: {timeline}");
    for kind in ["Labeled", "Assigned"] {
        assert!(has(&events, kind), "no {kind}: {timeline}");
    }
    assert!(
        events
            .iter()
            .any(|(kind, fields)| kind == "CrossReferenced" && fields["link"]["number"] == pull),
        "the pull request's cross-reference: {timeline}"
    );
}

#[test]
fn every_provider_reads_a_full_pull_request_conversation() {
    let mut failures = Vec::new();
    for forge in ALL {
        let world = World::with_state(forge, json!({ "conversation": true }));
        let (pull, issue) = numbers(forge);
        for (what, check) in [
            ("pull request", check_pull as fn(&World, Forge, u64, u64)),
            ("issue", check_issue),
        ] {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                check(&world, forge, pull, issue)
            }));
            if let Err(panic) = outcome {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_default();
                failures.push(format!("{} {what}: {message}", forge.name()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
