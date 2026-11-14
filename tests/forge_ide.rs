#![cfg(unix)]
//! The IDE's pull-request picker and inline review comments on the provider layer (FORGE.md
//! §5.2, F7), for GitHub, GitLab, Gitea and Forgejo: `ForgeJobs::{list, comments,
//! comment_target, comment}` against the fake forge, through detection and `connect` exactly as
//! the editor runs them. Each test re-runs itself in a child pointed at its own world.

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_world.rs"]
mod forge_world;

use dust::forge::ForgeJobs;
use dust::forge::ForgeReply;
use dust::forge::PullRequest;
use dust::forge::ReviewComment;
use forge_world::Forge;
use forge_world::World;
use forge_world::git;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const CHILD: &str = "DUSTY_FORGE_IDE_CHILD";

fn rerun_in_child(test: &str, world: &World) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([test, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, "1")
        .env("DUSTY_IDE_REPO", &world.repo);
    world.configure(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test} failed in its child:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child_repo() -> Option<PathBuf> {
    std::env::var_os(CHILD)?;
    Some(PathBuf::from(std::env::var_os("DUSTY_IDE_REPO").unwrap()))
}

fn wait(jobs: &ForgeJobs) -> ForgeReply {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = jobs.try_reply() {
            return event.reply;
        }
        assert!(Instant::now() < deadline, "the forge job never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn list(root: &Path) -> Vec<PullRequest> {
    let mut jobs = ForgeJobs::default();
    jobs.list(root.to_path_buf(), 1).unwrap();
    match wait(&jobs) {
        ForgeReply::List(Ok(list)) => list,
        other => panic!("the picker failed: {other:?}"),
    }
}

fn comments(root: &Path) -> Vec<ReviewComment> {
    let mut jobs = ForgeJobs::default();
    jobs.comments(root.to_path_buf(), 1).unwrap();
    match wait(&jobs) {
        ForgeReply::Comments(Ok(comments)) => comments,
        other => panic!("review comments failed: {other:?}"),
    }
}

/// The picker lists the open pull requests with their head branches on every provider.
fn picker(forge: Forge, test: &str, check: impl Fn(&[PullRequest])) {
    if let Some(root) = child_repo() {
        let list = list(&root);
        assert!(!list.is_empty());
        assert!(
            list.iter()
                .all(|pull| pull.number > 0 && !pull.title.is_empty())
        );
        check(&list);
        return;
    }
    let world = World::new(forge);
    rerun_in_child(test, &world);
    assert!(
        world
            .requests()
            .iter()
            .all(|request| request["method"] == "GET"
                || request["method"].is_null()
                || request["path"] == "graphql"),
        "the picker only reads"
    );
}

#[test]
fn github_picker_lists_pull_requests() {
    picker(Forge::GitHub, "github_picker_lists_pull_requests", |list| {
        let pull = list.iter().find(|pull| pull.number == 9).unwrap();
        assert_eq!(pull.title, "Native review");
        assert_eq!(pull.author, "alice");
        assert_eq!(pull.branch, "feature/native");
        assert!(!pull.draft);
    });
}

#[test]
fn gitlab_picker_lists_merge_requests() {
    picker(
        Forge::GitLab,
        "gitlab_picker_lists_merge_requests",
        |list| {
            let numbers = list.iter().map(|pull| pull.number).collect::<Vec<_>>();
            assert!(
                numbers.contains(&13) && numbers.contains(&11),
                "{numbers:?}"
            );
            assert!(
                !numbers.contains(&12),
                "merged requests are not open: {numbers:?}"
            );
            let pull = list.iter().find(|pull| pull.number == 13).unwrap();
            assert_eq!(pull.branch, "retry-uploads");
            assert_eq!(pull.author, "maya");
            assert!(list.iter().find(|pull| pull.number == 11).unwrap().draft);
        },
    );
}

#[test]
fn gitea_picker_lists_pull_requests() {
    picker(Forge::Gitea, "gitea_picker_lists_pull_requests", |list| {
        assert!(list.iter().any(|pull| pull.number == 1402), "{list:?}");
        assert!(list.iter().all(|pull| !pull.branch.is_empty()), "{list:?}");
    });
}

#[test]
fn forgejo_picker_lists_pull_requests() {
    picker(
        Forge::Forgejo,
        "forgejo_picker_lists_pull_requests",
        |list| {
            let pull = list.iter().find(|pull| pull.number == 1784).unwrap();
            assert_eq!(pull.branch, "docker-socket");
            assert!(!pull.author.is_empty());
        },
    );
}

/// GitHub end to end: the branch's pull request, its threads, a comment target pinned to the
/// head, the head/buffer guards, then the inline comment itself.
#[test]
fn github_inline_comments_pin_the_head() {
    if let Some(root) = child_repo() {
        let found = comments(&root);
        let first = found
            .iter()
            .find(|comment| comment.body == "Please keep this draft.")
            .unwrap_or_else(|| panic!("{found:?}"));
        assert_eq!(first.path, root.join("example.rs"));
        assert_eq!(first.line, Some(1));
        assert_eq!(first.author, "reviewer");
        let mut jobs = ForgeJobs::default();
        jobs.comment_target(
            root.clone(),
            1,
            root.join("example.rs"),
            1,
            "fn after() {}\n".into(),
        )
        .unwrap();
        let target = match wait(&jobs) {
            ForgeReply::CommentTarget(Ok(target)) => target,
            other => panic!("no comment target: {other:?}"),
        };
        assert_eq!(target.head.number, 9);
        assert_eq!(target.head.commit, git(&root, &["rev-parse", "HEAD"]));
        // An edited buffer is refused before anything is sent.
        let mut edited = target.clone();
        edited.text = "fn changed() {}\n".into();
        jobs.comment(root.clone(), 2, edited, "nit".into()).unwrap();
        match wait(&jobs) {
            ForgeReply::Commented(Err(error)) => assert!(error.contains("buffer"), "{error}"),
            other => panic!("an edited buffer was commented on: {other:?}"),
        }
        jobs.comment(root.clone(), 3, target, "Name this better.".into())
            .unwrap();
        match wait(&jobs) {
            ForgeReply::Commented(Ok(())) => {}
            other => panic!("the comment failed: {other:?}"),
        }
        return;
    }
    let world = World::new(Forge::GitHub);
    std::fs::write(world.repo.join("example.rs"), "fn after() {}\n").unwrap();
    git(&world.repo, &["add", "example.rs"]);
    git(&world.repo, &["commit", "-q", "-m", "after"]);
    git(&world.repo, &["checkout", "-q", "-b", "pr-9"]);
    let head = git(&world.repo, &["rev-parse", "HEAD"]);
    world.update(|state| state["prs"] = json!([{ "number": 9, "headRefOid": head }]));
    rerun_in_child("github_inline_comments_pin_the_head", &world);
    let state = world.state();
    let posted = state["inline_comments"].as_array().unwrap();
    assert_eq!(posted.len(), 1, "only the unchanged buffer is commented on");
    assert_eq!(posted[0]["path"], "example.rs");
    assert_eq!(posted[0]["line"], 1);
    assert_eq!(posted[0]["diffSide"], "RIGHT");
    assert_eq!(posted[0]["commit"]["oid"], head.as_str());
    assert_eq!(posted[0]["body"], "Name this better.");
}

/// GitLab: the branch's merge request is found by its source branch, and its diff notes come
/// back at their lines.
#[test]
fn gitlab_review_comments_follow_the_source_branch() {
    if let Some(root) = child_repo() {
        let found = comments(&root);
        let nit = found
            .iter()
            .find(|comment| comment.body == "nit: name the retry count")
            .unwrap_or_else(|| panic!("{found:?}"));
        assert_eq!(nit.path, root.join("src/lib.rs"));
        assert_eq!(nit.line, Some(12));
        assert_eq!(nit.author, "alex");
        assert!(
            found
                .iter()
                .any(|comment| comment.path == root.join("ci/retry.rs") && comment.line == Some(2))
        );
        return;
    }
    let world = World::new(Forge::GitLab);
    git(
        &world.repo,
        &["commit", "-q", "--allow-empty", "-m", "base"],
    );
    git(&world.repo, &["checkout", "-q", "-b", "retry-uploads"]);
    rerun_in_child("gitlab_review_comments_follow_the_source_branch", &world);
    assert!(
        world.requests().iter().any(|request| request["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/discussions")),
        "the threads came from the merge request's discussions"
    );
}

/// Gitea and Forgejo: Dust's own `pr-N` branch, and a fork checkout whose upstream is
/// `refs/pull/N/head` (the fork's branch has no pull request in the base repository).
fn gitea_threads(forge: Forge, test: &str, fork: bool) {
    if let Some(root) = child_repo() {
        comments(&root);
        return;
    }
    let world = World::new(forge);
    git(
        &world.repo,
        &["commit", "-q", "--allow-empty", "-m", "base"],
    );
    let number = forge.pull();
    if fork {
        git(&world.repo, &["checkout", "-q", "-b", "docker-socket"]);
        git(
            &world.repo,
            &["config", "branch.docker-socket.remote", "origin"],
        );
        git(
            &world.repo,
            &[
                "config",
                "branch.docker-socket.merge",
                &format!("refs/pull/{number}/head"),
            ],
        );
    } else {
        git(
            &world.repo,
            &["checkout", "-q", "-b", &format!("pr-{number}")],
        );
    }
    rerun_in_child(test, &world);
    let reviews = format!("repos/orbit/orbit/pulls/{number}/reviews");
    assert!(
        world
            .requests()
            .iter()
            .any(|request| request["path"].as_str().unwrap_or("").starts_with(&reviews)),
        "{:?}",
        world.requests()
    );
}

#[test]
fn gitea_review_comments_on_a_pr_branch() {
    gitea_threads(Forge::Gitea, "gitea_review_comments_on_a_pr_branch", false);
}

#[test]
fn forgejo_review_comments_on_a_fork_checkout() {
    gitea_threads(
        Forge::Forgejo,
        "forgejo_review_comments_on_a_fork_checkout",
        true,
    );
}

/// No forge at all: the picker falls back to the pull requests already checked out here.
#[test]
fn offline_picker_lists_local_checkouts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Test"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let checkout = directory.path().join("repo-pr-4");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr-4",
            checkout.to_str().unwrap(),
        ],
    );
    let mut jobs = ForgeJobs::default();
    jobs.list(root.clone(), 1).unwrap();
    match wait(&jobs) {
        ForgeReply::Offline { error, local } => {
            assert!(error.contains("remote"), "{error}");
            assert_eq!(local.len(), 1);
            assert_eq!(local[0].0, 4);
        }
        other => panic!("expected the offline list: {other:?}"),
    }
}
