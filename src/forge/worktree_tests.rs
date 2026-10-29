//! The review worktree loop, proved in a REAL repository: the inspected revision is recorded, the
//! current branch is untouched, and the review is returned to afterwards.

use std::path::Path;
use std::time::{Duration, Instant};

use super::review::ReviewRoom;
use super::worktree::{ReviewCheckout, checkout_request, leave_request};
use crate::workspace::git::{GitBranchRequest, GitBranchResult, GitBranchService};

fn drive(request: GitBranchRequest) -> Result<GitBranchResult, crate::workspace::git::GitBranchError> {
    let mut service = GitBranchService::default();
    service.start(request)?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match service.poll() {
            Ok(Some(result)) => return Ok(result.clone()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        assert!(Instant::now() < deadline, "the worktree request did not finish");
        std::thread::yield_now();
    }
}

/// A repository with `main` and a second branch carrying a commit that main does not have.
fn review_fixture(root: &Path) -> (git2::Oid, String, git2::Oid) {
    let repository = git2::Repository::init(root).unwrap();
    std::fs::write(root.join("main.txt"), "main\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("main.txt")).unwrap();
    index.write().unwrap();
    let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("dust", "dust@example.com").unwrap();
    let first = repository
        .commit(Some("HEAD"), &signature, &signature, "first", &tree, &[])
        .unwrap();
    let branch = repository.head().unwrap().shorthand().unwrap().to_owned();
    // The reviewed commit lives on its OWN branch: written with no parent update, and only its own
    // ref moved, so the current branch never moves for a review to exist.
    std::fs::write(root.join("review.txt"), "review\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("review.txt")).unwrap();
    index.write().unwrap();
    let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repository.find_commit(first).unwrap();
    let reviewed = repository
        .commit(None, &signature, &signature, "the reviewed change", &tree, &[&parent])
        .unwrap();
    repository
        .reference("refs/heads/review", reviewed, true, "review branch")
        .unwrap();
    drop(tree);
    drop(index);
    (reviewed, branch, first)
}

#[test]
fn a_review_worktree_records_its_revision_moves_no_branch_and_returns_to_the_review() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (reviewed, branch, main_head) = review_fixture(&root);

    let room = ReviewRoom::inspect(reviewed.to_string());
    let worktree = root.join("wt-review");

    // The checkout's revision comes FROM the room, so the two cannot disagree.
    drive(checkout_request(
        &room,
        &root,
        "review",
        &worktree,
        Duration::from_secs(5),
    ))
    .expect("the review's revision is checked out");

    // (1) THE INSPECTED REVISION IS RECORDED, and it is what the worktree actually holds.
    let checkout = ReviewCheckout::new("review", worktree.clone(), room.pinned().as_str());
    assert_eq!(checkout.revision, reviewed.to_string());
    let linked = git2::Repository::open(&worktree).unwrap();
    assert!(
        linked.head_detached().unwrap(),
        "a review checkout is detached at the inspected revision"
    );
    assert_eq!(linked.head().unwrap().target().unwrap(), reviewed);

    // (2) THE CURRENT BRANCH IS UNTOUCHED: same branch name, same commit, read from the repository.
    let repository = git2::Repository::open(&root).unwrap();
    assert_eq!(repository.head().unwrap().shorthand().unwrap(), branch);
    assert_eq!(repository.head().unwrap().target().unwrap(), main_head);

    // (3) THE REVIEW IS RETURNED TO: closing the worktree leaves the room pinning the same review.
    drive(leave_request(&root, "review", Duration::from_secs(5))).expect("the checkout closes");
    assert!(
        !worktree.exists(),
        "the review worktree is gone once the review is left"
    );
    assert_eq!(
        room.pinned().as_str(),
        reviewed.to_string(),
        "the room still inspects the same revision after the round trip"
    );
    // And the branch is still exactly where it was.
    assert_eq!(repository.head().unwrap().target().unwrap(), main_head);
}
