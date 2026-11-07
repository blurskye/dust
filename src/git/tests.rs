//! Tests for repository status, hunks and mutations.

use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::git::mutation::stage_hunk;

use crate::git::mutation::worktree_hash;
use crate::git::*;
use tempfile::tempdir;
fn commit(repo: &git2::Repository, path: &str, text: &str) {
    let root = repo.workdir().unwrap();
    std::fs::write(root.join(path), text).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new(path)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &parents)
        .unwrap();
}
#[test]
fn mutation_service_rejects_stale_worktree_wrong_hunk_and_suppresses_cancelled_late_work() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "negative.txt", "one\ntwo\nthree\n");
    std::fs::write(dir.path().join("negative.txt"), "one\nchanged\nthree\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 9,
        root_revision: 4,
        file: "negative.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let base = GitStageHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        expected_index: Some(snapshot.index_fingerprint),
        expected_worktree: snapshot.worktree_sha256,
        maximum_patch_bytes: 65536,
        deadline: Duration::from_secs(5),
    };

    let mut wrong = base.clone();
    wrong.selected.header.push_str(" stale");
    let mut service = GitMutationWorker::default();
    service.start(GitMutationRequest::StageHunk(wrong)).unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::WrongHunk
    );

    let mut stale_worktree = base.clone();
    stale_worktree.expected_worktree = Some([0; 32]);
    service
        .start(GitMutationRequest::StageHunk(stale_worktree.clone()))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::StaleWorktree
    );
    assert_eq!(
        stage_hunk(GitMutationRequestId(77), &base, &AtomicBool::new(true)),
        Err(GitMutationError::Cancelled)
    );
    service
        .start(GitMutationRequest::StageHunk(stale_worktree))
        .unwrap();
    service.cancel();
    let request_id = service.next;
    for _ in 0..256 {
        assert!(service.poll().unwrap().is_none());
        assert_eq!(service.next, request_id);
    }
}

#[test]
fn status_is_bounded_sorted_and_idle() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "tracked.txt", "one");
    std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join("new.txt"), "new").unwrap();
    let mut service = GitStatusWorker::default();
    service
        .start(GitStatusPolicy {
            root: dir.path().into(),
            root_revision: 3,
            maximum_files: 10,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(5),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if let Some(s) = service.poll().unwrap() {
            break s.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(
        snapshot.entries,
        vec![
            GitStatusEntry {
                original_path: None,
                relative_path: "new.txt".into(),
                status: GitFileStatus::Untracked,
                staged_status: None,
                unstaged_status: Some(GitFileStatus::Untracked),
                sources: GitChangeSources {
                    staged: false,
                    unstaged: true,
                    unsaved_document_revision: None,
                },
            },
            GitStatusEntry {
                original_path: None,
                relative_path: "tracked.txt".into(),
                status: GitFileStatus::Modified,
                staged_status: None,
                unstaged_status: Some(GitFileStatus::Modified),
                sources: GitChangeSources {
                    staged: false,
                    unstaged: true,
                    unsaved_document_revision: None,
                },
            }
        ]
    );
    for _ in 0..256 {
        assert!(service.snapshot_if_changed(snapshot.revision).is_none())
    }
}
fn status_policy(root: &Path) -> GitStatusPolicy {
    GitStatusPolicy {
        root: root.into(),
        root_revision: 1,
        maximum_files: 100,
        maximum_path_bytes: 4096,
        deadline: Duration::from_secs(5),
    }
}

fn read_status(root: &Path) -> GitStatusSnapshot {
    crate::git::status::read_status(
        GitStatusRequestId(1),
        &status_policy(root),
        &[],
        &AtomicBool::new(false),
    )
    .unwrap()
}

#[test]
fn status_paths_include_configured_worktree_below_workspace() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "file.txt", "one\n");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::rename(dir.path().join("file.txt"), checkout.join("file.txt")).unwrap();
    repo.set_workdir(&checkout, true).unwrap();
    std::fs::write(checkout.join("file.txt"), "two\n").unwrap();
    let snapshot = read_status(dir.path());
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(
        snapshot.entries[0].relative_path,
        Path::new("checkout/file.txt")
    );
    let decorations = aggregate_git_status(&snapshot.scope_root, &snapshot.entries);
    assert_eq!(
        decorations[&checkout.join("file.txt")],
        GitFileStatus::Modified
    );
}

#[test]
fn status_keeps_staged_rename_destination_after_worktree_moves_again() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "a", "same content\n");
    std::fs::rename(dir.path().join("a"), dir.path().join("b")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("a")).unwrap();
    index.add_path(Path::new("b")).unwrap();
    index.write().unwrap();
    std::fs::rename(dir.path().join("b"), dir.path().join("c")).unwrap();
    let snapshot = read_status(dir.path());
    let staged = snapshot
        .entries
        .iter()
        .find(|entry| entry.sources.staged)
        .unwrap();
    assert_eq!(staged.relative_path, Path::new("b"));
    assert_eq!(staged.original_path.as_deref(), Some(Path::new("a")));
    assert_eq!(staged.staged_status, Some(GitFileStatus::Renamed));
    let preview = read_hunks(&plan_policy(dir.path(), "b", GitDiffLayer::Staged)).unwrap();
    assert!(!preview.hunks.is_empty());
    let later = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == Path::new("c"))
        .unwrap();
    assert!(!later.sources.staged);
}

#[test]
fn status_nested_scope_retains_removal_for_outgoing_rename() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    commit(&repo, "sub/a", "same content\n");
    std::fs::rename(dir.path().join("sub/a"), dir.path().join("outside")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("sub/a")).unwrap();
    index.add_path(Path::new("outside")).unwrap();
    index.write().unwrap();
    let snapshot = read_status(&dir.path().join("sub"));
    assert_eq!(snapshot.entries.len(), 1);
    let entry = &snapshot.entries[0];
    assert_eq!(entry.relative_path, Path::new("a"));
    assert_eq!(entry.staged_status, Some(GitFileStatus::Deleted));
    assert_eq!(entry.status, GitFileStatus::Deleted);
    assert!(entry.sources.staged);
}

#[test]
fn status_path_budget_includes_rename_source() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "longname", "same\n");
    std::fs::rename(dir.path().join("longname"), dir.path().join("b")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("longname")).unwrap();
    index.add_path(Path::new("b")).unwrap();
    index.write().unwrap();
    let mut policy = status_policy(dir.path());
    policy.maximum_path_bytes = 1;
    let snapshot = crate::git::status::read_status(
        GitStatusRequestId(1),
        &policy,
        &[],
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(snapshot.truncated);
    assert!(snapshot.entries.is_empty());
}

#[test]
fn status_change_codes_keep_index_and_worktree_kinds() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "base", "base\n");
    std::fs::write(dir.path().join("new"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("new")).unwrap();
    index.write().unwrap();
    std::fs::write(dir.path().join("new"), "unstaged\n").unwrap();
    let snapshot = read_status(dir.path());
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == Path::new("new"))
        .unwrap();
    assert_eq!(entry.staged_status, Some(GitFileStatus::Added));
    assert_eq!(entry.unstaged_status, Some(GitFileStatus::Modified));
    assert_eq!(entry.change_code(), "AM");
}

#[test]
fn unsaved_revision_lookup_handles_large_sorted_lists_and_misses() {
    let documents = (0..50_000)
        .map(|index| GitUnsavedDocument::new(PathBuf::from(format!("file-{index:05}")), index + 1))
        .collect::<Vec<_>>();
    validate_unsaved_documents(&documents, 100_000, 16 * 1024 * 1024).unwrap();
    for index in [0, 1, 25_000, 49_999] {
        assert_eq!(
            unsaved_document_revision(&documents, Path::new(&format!("file-{index:05}"))),
            Some(index + 1)
        );
    }
    for index in 0..50_000 {
        assert_eq!(
            unsaved_document_revision(&documents, Path::new(&format!("missing-{index:05}"))),
            None
        );
    }
}

#[test]
fn status_keeps_index_and_worktree_codes_independent() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "base.txt", "base\n");
    std::fs::write(dir.path().join("added.txt"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("added.txt")).unwrap();
    index.write().unwrap();
    std::fs::write(dir.path().join("added.txt"), "modified after staging\n").unwrap();
    let snapshot = crate::git::status::read_status(
        GitStatusRequestId(1),
        &GitStatusPolicy {
            root: dir.path().into(),
            root_revision: 1,
            maximum_files: 10,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(5),
        },
        &[],
        &AtomicBool::new(false),
    )
    .unwrap();
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == Path::new("added.txt"))
        .unwrap();
    assert_eq!(entry.change_code(), "AM");
    assert!(entry.sources.staged && entry.sources.unstaged);
}

#[cfg(unix)]
#[test]
fn status_change_codes_preserve_type_changes_on_both_sides() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "changed.txt", "regular\n");
    let path = dir.path().join("changed.txt");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("target.txt", &path).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("changed.txt")).unwrap();
    index.write().unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "regular again\n").unwrap();
    let snapshot = read_status(dir.path());
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == Path::new("changed.txt"))
        .unwrap();
    assert_eq!(entry.staged_status, Some(GitFileStatus::TypeChanged));
    assert_eq!(entry.unstaged_status, Some(GitFileStatus::TypeChanged));
    assert_eq!(entry.change_code(), "TT");
}

#[test]
fn worktree_hash_rejects_oversized_files_before_reading() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("large.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    assert!(matches!(
        worktree_hash(&path),
        Err(GitMutationError::Invalid)
    ));
}

#[test]
fn nested_root_is_scoped_and_limits_truncate_results() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    commit(&repo, "nested/tracked.txt", "one");
    std::fs::write(dir.path().join("sibling.txt"), "outside").unwrap();
    std::fs::write(dir.path().join("nested/tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join("nested/new.txt"), "new").unwrap();
    let mut service = GitStatusWorker::default();
    service
        .start(GitStatusPolicy {
            root: dir.path().join("nested"),
            root_revision: 4,
            maximum_files: 1,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(5),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if let Some(s) = service.poll().unwrap() {
            break s.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert!(snapshot.truncated);
    assert_eq!(snapshot.entries.len(), 1);
    assert!(
        snapshot
            .entries
            .iter()
            .all(|e| !e.relative_path.starts_with("sibling.txt"))
    );
    service
        .start(GitStatusPolicy {
            root: dir.path().join("nested"),
            root_revision: 4,
            maximum_files: 10,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(5),
        })
        .unwrap();
    let scoped = wait_status(&mut service);
    assert!(!scoped.truncated);
    assert_eq!(
        scoped
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_path())
            .collect::<Vec<_>>(),
        vec![Path::new("new.txt"), Path::new("tracked.txt")]
    );
    assert_eq!(
        GitStatusWorker::default()
            .start(GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 1,
                maximum_files: 0,
                maximum_path_bytes: 1,
                deadline: Duration::from_secs(1)
            })
            .unwrap_err()
            .to_string(),
        "invalid git status policy"
    );
}
#[test]
fn unstaged_unicode_hunks_are_bounded_and_revision_owned() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "u.txt", "alpha βeta\nlast\n");
    let workdir = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    let repository_id = GitRepositoryId(hasher.finish());
    std::fs::write(dir.path().join("u.txt"), "alpha γamma\nlast\n").unwrap();
    let policy = GitHunkPolicy {
        scope_root: workdir,
        repository_id,
        status_revision: 4,
        root_revision: 3,
        file: "u.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let mut service = GitHunkWorker::default();
    let request_id = service.start(policy.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if let Some(snapshot) = service.poll().unwrap() {
            break snapshot.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(snapshot.request_id, request_id);
    for _ in 0..256 {
        assert!(service.snapshot_if_changed(snapshot.revision).is_none())
    }
    assert_eq!(snapshot.status_revision, 4);
    assert_eq!(snapshot.hunks.len(), 1);
    assert!(
        snapshot.hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == GitDiffLineKind::Add && line.text.contains("γamma"))
    );
    let mut tiny = policy;
    tiny.maximum_lines = 1;
    assert_eq!(read_hunks(&tiny), Err(GitHunkError::Limit));
}
#[test]
fn empty_hunks_still_obey_the_request_deadline_and_cancellation() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "same.txt", "same\n");
    let mut policy = plan_policy(dir.path(), "same.txt", GitDiffLayer::Unstaged);
    let diff = repo.diff_index_to_workdir(None, None).unwrap();
    let patch = git2::Patch::from_buffers(b"same\n", None, b"same\n", None, None).unwrap();
    let expired = Instant::now() - policy.deadline;
    for cancel in [false, true] {
        let cancelled = AtomicBool::new(cancel);
        let expected = if cancel {
            GitHunkError::Cancelled
        } else {
            GitHunkError::Deadline
        };
        assert_eq!(
            hunks::collect_hunks_from_diff(&diff, &policy, &cancelled, expired),
            Err(expected)
        );
        assert_eq!(
            hunks::collect_hunks_from_patch(&patch, &policy, &cancelled, expired),
            Err(if cancel {
                GitHunkError::Cancelled
            } else {
                GitHunkError::Deadline
            })
        );
    }
    policy.deadline = Duration::from_nanos(1);
    assert_eq!(read_hunks(&policy), Err(GitHunkError::Deadline));
    assert_eq!(
        hunks::read_buffer_hunks_owned(
            GitHunkRequestId(0),
            &policy,
            GitHunkBufferInput {
                revision: 1,
                bytes: b"same\n".to_vec()
            },
            &AtomicBool::new(false),
        ),
        Err(GitHunkError::Deadline)
    );
}

#[test]
fn buffer_hunks_refuse_unmerged_paths_but_allow_untracked_files() {
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    let policy = plan_policy(dir.path(), "shared.txt", GitDiffLayer::Unstaged);
    let input = GitHunkBufferInput {
        revision: 1,
        bytes: b"resolved\n".to_vec(),
    };
    assert_eq!(
        hunks::read_buffer_hunks_owned(
            GitHunkRequestId(0),
            &policy,
            input.clone(),
            &AtomicBool::new(false)
        ),
        Err(GitHunkError::Unmerged)
    );
    let untracked = hunks::read_buffer_hunks_owned(
        GitHunkRequestId(0),
        &GitHunkPolicy {
            file: "new.txt".into(),
            ..policy
        },
        input,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        git_hunk_signs(&untracked),
        vec![GitHunkSign {
            line: 0,
            kind: GitHunkSignKind::Added
        }]
    );
}

#[test]
fn buffer_hunks_compare_exact_unsaved_revision_to_index_without_worktree_mutation() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "live.txt", "one\ntwo\nthree\n");
    let workdir = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: workdir,
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 9,
        root_revision: 4,
        file: "live.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    assert!(read_hunks(&policy).unwrap().hunks.is_empty());
    let mut service = GitHunkWorker::default();
    service
        .start_buffer(
            policy.clone(),
            GitHunkBufferInput {
                revision: 7,
                bytes: b"one\nthree\n".to_vec(),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if let Some(snapshot) = service.poll().unwrap() {
            break snapshot.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(snapshot.buffer_revision, Some(7));
    assert_eq!(snapshot.worktree_sha256, None);
    assert!(
        snapshot
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.kind == GitDiffLineKind::Delete && line.text == "two\n")
    );
    assert!(
        git_hunk_signs(&snapshot)
            .iter()
            .any(|sign| sign.kind == GitHunkSignKind::Deleted)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("live.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert_eq!(
        service.start_buffer(
            GitHunkPolicy {
                layer: GitDiffLayer::Staged,
                ..policy.clone()
            },
            GitHunkBufferInput {
                revision: 8,
                bytes: Vec::new(),
            },
        ),
        Err(GitHunkError::InvalidPolicy)
    );
    assert_eq!(
        service.start_buffer(
            GitHunkPolicy {
                maximum_bytes: 1,
                ..policy
            },
            GitHunkBufferInput {
                revision: 8,
                bytes: b"xx".to_vec(),
            },
        ),
        Err(GitHunkError::Limit)
    );
}

#[test]
fn libgit2_can_stage_one_exact_hunk_without_changing_worktree() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "two.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    std::fs::write(dir.path().join("two.txt"), &changed).unwrap();
    let workdir = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: workdir,
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 1,
        root_revision: 1,
        file: "two.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    assert_eq!(snapshot.hunks.len(), 2);
    let h = &snapshot.hunks[0];
    let mut patch = format!(
        "diff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n{}",
        h.header
    );
    for line in &h.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '+',
            GitDiffLineKind::Delete => '-',
        });
        patch.push_str(&line.text)
    }
    let diff = git2::Diff::from_buffer(patch.as_bytes()).unwrap();
    repo.apply(&diff, git2::ApplyLocation::Index, None).unwrap();
    let staged = read_hunks(&GitHunkPolicy {
        layer: GitDiffLayer::Staged,
        ..policy.clone()
    })
    .unwrap();
    let unstaged = read_hunks(&policy).unwrap();
    assert_eq!(staged.hunks.len(), 1);
    assert_eq!(unstaged.hunks.len(), 1);
    assert!(
        unstaged.hunks[0]
            .lines
            .iter()
            .any(|l| l.text.contains("changed 18"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("two.txt")).unwrap(),
        changed
    );
}
#[test]
fn mutation_service_stages_exact_hunk_and_rejects_stale_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "m.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    std::fs::write(dir.path().join("m.txt"), &changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 1,
        root_revision: 1,
        file: "m.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let index = git_index_fingerprint(&root).unwrap();
    let request = GitStageHunkRequest {
        policy: policy.clone(),
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        expected_index: Some(index),
        expected_worktree: snapshot.worktree_sha256,
        maximum_patch_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    };
    let mut service = GitMutationWorker::default();
    let request_id = service
        .start(GitMutationRequest::StageHunk(request.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let result = loop {
        if let Some(result) = service.poll().unwrap() {
            break result.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(result.request_id, request_id);
    assert_eq!(result.kind, GitMutationKind::StageHunk);
    assert_ne!(result.before, result.after);
    assert_eq!(
        std::fs::read_to_string(root.join("m.txt")).unwrap(),
        changed
    );
    assert_eq!(
        index_text(&root, "m.txt"),
        original.replace("line 2\n", "changed 2\n")
    );
    let unstaged = read_hunks(&policy).unwrap();
    assert_eq!(unstaged.hunks.len(), 1);
    assert!(
        unstaged.hunks[0]
            .lines
            .iter()
            .any(|line| { line.kind == GitDiffLineKind::Add && line.text == "changed 18\n" })
    );
    let mut stale = request;
    stale.expected_index = Some(index);
    let current = git_index_fingerprint(&root).unwrap();
    let mut second = GitMutationWorker::default();
    second.start(GitMutationRequest::StageHunk(stale)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match second.poll() {
            Err(GitMutationError::StaleIndex) => break,
            Ok(_) => {}
            Err(e) => panic!("{e}"),
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(git_index_fingerprint(&root).unwrap(), current);
}
#[test]
fn libgit2_can_reset_one_exact_hunk_without_changing_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "reset.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    std::fs::write(dir.path().join("reset.txt"), &changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 1,
        root_revision: 1,
        file: "reset.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    assert_eq!(snapshot.hunks.len(), 2);
    let h = &snapshot.hunks[0];
    let mut patch = format!(
        "diff --git a/reset.txt b/reset.txt\n--- a/reset.txt\n+++ b/reset.txt\n@@ -{},{} +{},{} @@\n",
        h.new_start, h.new_lines, h.old_start, h.old_lines
    );
    for line in &h.lines {
        patch.push(match line.kind {
            GitDiffLineKind::Context => ' ',
            GitDiffLineKind::Add => '-',
            GitDiffLineKind::Delete => '+',
        });
        patch.push_str(&line.text)
    }
    let before = git_index_fingerprint(&root).unwrap();
    let diff = git2::Diff::from_buffer(patch.as_bytes()).unwrap();
    repo.apply(&diff, git2::ApplyLocation::WorkDir, None)
        .unwrap();
    assert_eq!(git_index_fingerprint(&root).unwrap(), before);
    let remaining = read_hunks(&policy).unwrap();
    assert_eq!(remaining.hunks.len(), 1);
    assert!(
        remaining.hunks[0]
            .lines
            .iter()
            .any(|line| line.text.contains("changed 18"))
    );
    assert!(
        std::fs::read_to_string(root.join("reset.txt"))
            .unwrap()
            .contains("line 2\n")
    );
    assert!(
        read_hunks(&GitHunkPolicy {
            layer: GitDiffLayer::Staged,
            ..policy
        })
        .unwrap()
        .hunks
        .is_empty()
    );
}
fn wait_mutation_error(service: &mut GitMutationWorker) -> GitMutationError {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Err(error) = service.poll() {
            return error;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
}

#[test]
fn mutation_service_rejects_stale_worktree_wrong_hunk_and_cancelled_requests() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "negative.txt", "one\ntwo\nthree\n");
    std::fs::write(dir.path().join("negative.txt"), "one\nchanged\nthree\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 9,
        root_revision: 4,
        file: "negative.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let base = GitStageHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        expected_index: Some(snapshot.index_fingerprint),
        expected_worktree: snapshot.worktree_sha256,
        maximum_patch_bytes: 65536,
        deadline: Duration::from_secs(5),
    };

    let mut wrong = base.clone();
    wrong.selected.header.push_str(" stale");
    let mut service = GitMutationWorker::default();
    service.start(GitMutationRequest::StageHunk(wrong)).unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::WrongHunk
    );

    let mut stale_worktree = base.clone();
    stale_worktree.expected_worktree = Some([0; 32]);
    service
        .start(GitMutationRequest::StageHunk(stale_worktree.clone()))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::StaleWorktree
    );
    assert_eq!(
        stage_hunk(GitMutationRequestId(77), &base, &AtomicBool::new(true)),
        Err(GitMutationError::Cancelled)
    );
}

#[cfg(unix)]
#[test]
fn mutation_service_rejects_symlink_root_replacement_binary_and_directory_paths() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "valid.txt", "one\ntwo\n");
    std::fs::write(dir.path().join("valid.txt"), "one\nchanged\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let repository_id = GitRepositoryId(hasher.finish());
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id,
        status_revision: 3,
        root_revision: 2,
        file: "valid.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let request_for = |policy: GitHunkPolicy, worktree: [u8; 32]| GitStageHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        expected_index: Some(snapshot.index_fingerprint),
        expected_worktree: Some(worktree),
        maximum_patch_bytes: 65536,
        deadline: Duration::from_secs(5),
    };
    let mut service = GitMutationWorker::default();

    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("outside.txt"), "changed\n").unwrap();
    symlink(outside.path().join("outside.txt"), root.join("linked.txt")).unwrap();
    let mut symlink_policy = policy.clone();
    symlink_policy.file = "linked.txt".into();
    let linked_hash = worktree_hash(&outside.path().join("outside.txt"))
        .unwrap()
        .0;
    assert!(worktree_hash(&root.join("linked.txt")).is_err());
    service
        .start(GitMutationRequest::StageHunk(request_for(
            symlink_policy,
            linked_hash,
        )))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::OutsideScope
    );

    let replacement = tempdir().unwrap();
    git2::Repository::init(replacement.path()).unwrap();
    let mut replacement_policy = policy.clone();
    replacement_policy.scope_root = crate::files::canonicalize(replacement.path()).unwrap();
    service
        .start(GitMutationRequest::StageHunk(request_for(
            replacement_policy,
            snapshot.worktree_sha256.unwrap(),
        )))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::StaleRepository
    );

    std::fs::create_dir(root.join("directory")).unwrap();
    let mut directory_policy = policy.clone();
    directory_policy.file = "directory".into();
    service
        .start(GitMutationRequest::StageHunk(request_for(
            directory_policy,
            snapshot.worktree_sha256.unwrap(),
        )))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::UnsupportedPath
    );

    std::fs::write(root.join("binary.bin"), "plain\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("binary.bin")).unwrap();
    index.write().unwrap();
    std::fs::write(root.join("binary.bin"), b"\0binary\0").unwrap();
    let mut binary_policy = policy;
    binary_policy.file = "binary.bin".into();
    let binary_hash = worktree_hash(&root.join("binary.bin")).unwrap().0;
    let mut binary_request = request_for(binary_policy, binary_hash);
    binary_request.expected_index = Some(git_index_fingerprint(&root).unwrap());
    service
        .start(GitMutationRequest::StageHunk(binary_request))
        .unwrap();
    assert_eq!(
        wait_mutation_error(&mut service),
        GitMutationError::UnsupportedPath
    );
}

#[test]
fn git_status_aggregation_and_hunk_signs_are_deterministic_and_typed() {
    let root = PathBuf::from("/workspace");
    let entries = vec![
        GitStatusEntry {
            original_path: None,
            relative_path: "src/a.rs".into(),
            status: GitFileStatus::Modified,
            staged_status: None,
            unstaged_status: Some(GitFileStatus::Modified),
            sources: GitChangeSources {
                staged: false,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            original_path: None,
            relative_path: "src/nested/b.rs".into(),
            status: GitFileStatus::Conflicted,
            staged_status: Some(GitFileStatus::Conflicted),
            unstaged_status: Some(GitFileStatus::Conflicted),
            sources: GitChangeSources {
                staged: true,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            original_path: None,
            relative_path: "docs/readme.md".into(),
            status: GitFileStatus::Untracked,
            staged_status: None,
            unstaged_status: Some(GitFileStatus::Untracked),
            sources: GitChangeSources {
                staged: false,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            original_path: None,
            relative_path: "../escape".into(),
            status: GitFileStatus::Conflicted,
            staged_status: Some(GitFileStatus::Conflicted),
            unstaged_status: Some(GitFileStatus::Conflicted),
            sources: GitChangeSources {
                staged: true,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
    ];
    let aggregate = aggregate_git_status(&root, &entries);
    assert_eq!(aggregate[&root.join("src")], GitFileStatus::Conflicted);
    assert_eq!(aggregate[&root], GitFileStatus::Conflicted);
    assert_eq!(aggregate[&root.join("docs")], GitFileStatus::Untracked);
    assert!(!aggregate.contains_key(&root.join("../escape")));

    let snapshot = GitHunkSnapshot {
        request_id: GitHunkRequestId(1),
        revision: 1,
        repository_id: GitRepositoryId(1),
        status_revision: 1,
        root_revision: 1,
        file: "a".into(),
        layer: GitDiffLayer::Unstaged,
        buffer_revision: None,
        index_fingerprint: GitIndexFingerprint([0; 32]),
        worktree_sha256: Some([1; 32]),
        hunks: vec![
            GitHunk {
                old_mode: 0o100644,
                new_mode: 0o100644,
                id: GitHunkId(1),
                old_start: 1,
                old_lines: 0,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![GitDiffLine {
                    kind: GitDiffLineKind::Add,
                    old_line: None,
                    new_line: Some(1),
                    text: "add\n".into(),
                }],
            },
            GitHunk {
                old_mode: 0o100644,
                new_mode: 0o100644,
                id: GitHunkId(2),
                old_start: 3,
                old_lines: 1,
                new_start: 3,
                new_lines: 1,
                header: String::new(),
                lines: vec![
                    GitDiffLine {
                        kind: GitDiffLineKind::Delete,
                        old_line: Some(3),
                        new_line: None,
                        text: "old\n".into(),
                    },
                    GitDiffLine {
                        kind: GitDiffLineKind::Add,
                        old_line: None,
                        new_line: Some(3),
                        text: "new\n".into(),
                    },
                ],
            },
            GitHunk {
                old_mode: 0o100644,
                new_mode: 0o100644,
                id: GitHunkId(3),
                old_start: 5,
                old_lines: 1,
                new_start: 5,
                new_lines: 0,
                header: String::new(),
                lines: vec![GitDiffLine {
                    kind: GitDiffLineKind::Delete,
                    old_line: Some(5),
                    new_line: None,
                    text: "gone\n".into(),
                }],
            },
        ],
    };
    assert_eq!(
        git_hunk_signs(&snapshot),
        vec![
            GitHunkSign {
                line: 0,
                kind: GitHunkSignKind::Added
            },
            GitHunkSign {
                line: 2,
                kind: GitHunkSignKind::Changed
            },
            GitHunkSign {
                line: 4,
                kind: GitHunkSignKind::Deleted
            },
        ]
    );
}

#[test]
fn worktree_hash_bounds_reads_and_rejects_non_regular_files() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("large");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    assert_eq!(worktree_hash(&path), Err(GitMutationError::Invalid));
    file.set_len(8 * 1024 * 1024).unwrap();
    assert_eq!(worktree_hash(&path).unwrap().1.len(), 8 * 1024 * 1024);
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert_eq!(worktree_hash(&path), Err(GitMutationError::Invalid));
    assert_eq!(
        worktree_hash(dir.path()),
        Err(GitMutationError::UnsupportedPath)
    );
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(worktree_hash(&link), Err(GitMutationError::UnsupportedPath));
        let fifo = dir.path().join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(worktree_hash(&fifo), Err(GitMutationError::UnsupportedPath));
    }
}

#[test]
fn reset_service_restores_only_selected_hunk_and_preserves_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "service-reset.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    std::fs::write(dir.path().join("service-reset.txt"), &changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let policy = GitHunkPolicy {
        scope_root: root.clone(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 1,
        root_revision: 1,
        file: "service-reset.txt".into(),
        layer: GitDiffLayer::Unstaged,
        maximum_hunks: 10,
        maximum_lines: 100,
        maximum_bytes: 4096,
        deadline: Duration::from_secs(5),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let before = git_index_fingerprint(&root).unwrap();
    let hash = worktree_hash(&root.join("service-reset.txt")).unwrap().0;
    let request = GitResetHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        selected_lines: Vec::new(),
        expected_index: Some(before),
        expected_worktree: Some(hash),
        maximum_patch_bytes: 65536,
        deadline: Duration::from_secs(5),
    };
    let mut service = GitMutationWorker::default();
    service
        .start(GitMutationRequest::ResetUnstagedHunk(request))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let result = loop {
        if let Some(result) = service.poll().unwrap() {
            break result.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(result.kind, GitMutationKind::ResetUnstagedHunk);
    assert_eq!(result.before, result.after);
    assert_eq!(git_index_fingerprint(&root).unwrap(), before);
    let text = std::fs::read_to_string(root.join("service-reset.txt")).unwrap();
    assert!(text.contains("line 2\n") && text.contains("changed 18\n"));
}

fn plan_policy(root: &Path, file: &str, layer: GitDiffLayer) -> GitHunkPolicy {
    let root = crate::files::canonicalize(root).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    GitHunkPolicy {
        scope_root: root.to_path_buf(),
        repository_id: GitRepositoryId(hasher.finish()),
        status_revision: 1,
        root_revision: 1,
        file: file.into(),
        layer,
        maximum_hunks: 10,
        maximum_lines: 1000,
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    }
}

#[cfg(unix)]
#[test]
fn plan_policy_uses_canonical_repository_identity() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("repo");
    let repo = git2::Repository::init(&root).unwrap();
    commit(&repo, "a.txt", "one\n");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let policy = plan_policy(&alias, "a.txt", GitDiffLayer::Unstaged);
    assert_eq!(
        policy.scope_root,
        crate::files::canonicalize(&root).unwrap()
    );
    assert!(read_hunks(&policy).unwrap().hunks.is_empty());
}

fn plan_template(policy: GitHunkPolicy, kind: GitMutationKind) -> GitChangePlanRequest {
    GitChangePlanRequest {
        kind,
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: None,
        selected: None,
        selected_lines: Vec::new(),
        expected_index: None,
        expected_worktree: None,
        expected_head: GitHeadExpectation::Uncaptured,
        expected_document_revision: None,
        unsaved_documents: Vec::new(),
        maximum_patch_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    }
}

fn run_plan(plan: GitChangePlanRequest) -> Result<GitMutationResult, GitMutationError> {
    let mut service = GitMutationWorker::default();
    service.start(GitMutationRequest::Plan(plan)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match service.poll() {
            Ok(Some(result)) => return Ok(result.clone()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        assert!(Instant::now() < deadline, "plan did not finish");
        thread::yield_now();
    }
}

/// Read the on-disk index through a fresh repository handle. A handle that already created an
/// index caches it in memory, so reusing one would read a stale projection.
fn index_text(root: &Path, path: &str) -> String {
    let repo = git2::Repository::open(root).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(Path::new(path), 0).unwrap();
    let blob = repo.find_blob(entry.id).unwrap();
    String::from_utf8(blob.content().to_vec()).unwrap()
}

fn wait_status(service: &mut GitStatusWorker) -> GitStatusSnapshot {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(snapshot) = service.poll().unwrap() {
            return snapshot.clone();
        }
        assert!(Instant::now() < deadline, "status did not finish");
        thread::yield_now();
    }
}

#[test]
fn change_sources_distinguish_staged_unstaged_and_unsaved() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "staged.txt", "one\n");
    commit(&repo, "unstaged.txt", "one\n");
    commit(&repo, "dirty.txt", "one\n");
    std::fs::write(dir.path().join("staged.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    std::fs::write(dir.path().join("unstaged.txt"), "two\n").unwrap();
    let mut service = GitStatusWorker::default();
    service
        .start_with_unsaved(
            GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 1,
                maximum_files: 64,
                maximum_path_bytes: 4096,
                deadline: Duration::from_secs(5),
            },
            vec![GitUnsavedDocument::new("dirty.txt".into(), 7)],
        )
        .unwrap();
    let snapshot = wait_status(&mut service);
    let entry = |name: &str| {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.relative_path == Path::new(name))
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    assert_eq!(
        entry("staged.txt").sources,
        GitChangeSources {
            staged: true,
            unstaged: false,
            unsaved_document_revision: None,
        }
    );
    assert_eq!(
        entry("unstaged.txt").sources,
        GitChangeSources {
            staged: false,
            unstaged: true,
            unsaved_document_revision: None,
        }
    );
    // A path whose only change is an unsaved buffer still appears, labelled as that source.
    assert_eq!(
        entry("dirty.txt").sources,
        GitChangeSources {
            staged: false,
            unstaged: false,
            unsaved_document_revision: Some(7),
        }
    );
    assert!(
        entry("dirty.txt")
            .sources
            .contains(GitChangeSource::Unsaved {
                document_revision: 7
            })
    );
    assert!(
        !entry("dirty.txt")
            .sources
            .contains(GitChangeSource::Unstaged)
    );
    assert!(matches!(
        service.start_with_unsaved(
            GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 1,
                maximum_files: 64,
                maximum_path_bytes: 4096,
                deadline: Duration::from_secs(5),
            },
            vec![GitUnsavedDocument::new("dirty.txt".into(), 0)],
        ),
        Err(GitStatusError::InvalidPolicy)
    ));
}

#[test]
fn hunk_snapshot_source_reports_staged_unstaged_and_unsaved() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "src.txt", "one\ntwo\nthree\n");
    std::fs::write(dir.path().join("src.txt"), "one\nchanged\nthree\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let unstaged = plan_policy(&root, "src.txt", GitDiffLayer::Unstaged);
    assert_eq!(
        read_hunks(&unstaged).unwrap().source(),
        GitChangeSource::Unstaged
    );
    let staged = plan_policy(&root, "src.txt", GitDiffLayer::Staged);
    assert_eq!(
        read_hunks(&staged).unwrap().source(),
        GitChangeSource::Staged
    );
    let mut service = GitHunkWorker::default();
    service
        .start_buffer(
            plan_policy(&root, "src.txt", GitDiffLayer::Unstaged),
            GitHunkBufferInput {
                revision: 11,
                bytes: b"one\nbuffer\nthree\n".to_vec(),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if let Some(snapshot) = service.poll().unwrap() {
            break snapshot.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(
        snapshot.source(),
        GitChangeSource::Unsaved {
            document_revision: 11
        }
    );
    assert_eq!(snapshot.buffer_revision, Some(11));
    let _ = repo;
}

#[test]
fn staging_a_path_with_unsaved_edits_is_refused_before_any_write() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "guard.txt", "one\n");
    std::fs::write(dir.path().join("guard.txt"), "two\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let index_before = git_index_fingerprint(&root).unwrap();
    let mut plan = plan_template(
        plan_policy(&root, "guard.txt", GitDiffLayer::Unstaged),
        GitMutationKind::StageFile,
    );
    plan.expected_index = Some(index_before);
    plan.expected_worktree = Some(worktree_hash(&root.join("guard.txt")).unwrap().0);
    plan.unsaved_documents = vec![GitUnsavedDocument::new("guard.txt".into(), 3)];
    assert_eq!(
        run_plan(plan.clone()),
        Err(GitMutationError::UnsavedDocument)
    );
    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
    assert_eq!(index_text(&root, "guard.txt"), "one\n");
    // A captured document revision that no longer matches is rejected before the write too.
    plan.unsaved_documents.clear();
    plan.expected_document_revision = Some(3);
    assert_eq!(run_plan(plan), Err(GitMutationError::StaleDocument));
    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
}

#[test]
fn file_level_stage_and_unstage_are_separately_named() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "whole.txt", "one\n");
    std::fs::write(dir.path().join("whole.txt"), "two\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let head = git_head_id(&root).unwrap();
    let mut stage = plan_template(
        plan_policy(&root, "whole.txt", GitDiffLayer::Unstaged),
        GitMutationKind::StageFile,
    );
    stage.expected_index = Some(git_index_fingerprint(&root).unwrap());
    stage.expected_worktree = Some(worktree_hash(&root.join("whole.txt")).unwrap().0);
    stage.expected_head = GitHeadExpectation::At(head.unwrap());
    let staged = run_plan(stage).unwrap();
    assert_eq!(staged.kind, GitMutationKind::StageFile);
    assert_eq!(staged.hunk, None);
    assert_eq!(staged.path, PathBuf::from("whole.txt"));
    assert_ne!(staged.before, staged.after);
    assert_eq!(index_text(&root, "whole.txt"), "two\n");
    assert_eq!(
        std::fs::read_to_string(root.join("whole.txt")).unwrap(),
        "two\n"
    );
    let mut unstage = plan_template(
        plan_policy(&root, "whole.txt", GitDiffLayer::Unstaged),
        GitMutationKind::UnstageFile,
    );
    unstage.expected_index = Some(git_index_fingerprint(&root).unwrap());
    let unstaged = run_plan(unstage).unwrap();
    assert_eq!(unstaged.kind, GitMutationKind::UnstageFile);
    assert_eq!(index_text(&root, "whole.txt"), "one\n");
    // Unstaging never writes the worktree.
    assert_eq!(
        std::fs::read_to_string(root.join("whole.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn plan_is_rejected_after_an_external_index_or_head_change() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "stale.txt", "one\n");
    commit(&repo, "other.txt", "one\n");
    std::fs::write(dir.path().join("stale.txt"), "two\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let head = git_head_id(&root).unwrap();
    let mut plan = plan_template(
        plan_policy(&root, "stale.txt", GitDiffLayer::Unstaged),
        GitMutationKind::StageFile,
    );
    plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
    plan.expected_worktree = Some(worktree_hash(&root.join("stale.txt")).unwrap().0);
    plan.expected_head = GitHeadExpectation::At(head.unwrap());
    // An external index change invalidates the captured index.
    std::fs::write(dir.path().join("other.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("other.txt")).unwrap();
    index.write().unwrap();
    assert_eq!(run_plan(plan.clone()), Err(GitMutationError::StaleIndex));
    assert_eq!(index_text(&root, "stale.txt"), "one\n");
    // An external commit invalidates the captured HEAD even with a fresh index capture.
    plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "external", &tree, &[&parent])
        .unwrap();
    assert_eq!(run_plan(plan), Err(GitMutationError::StaleHead));
    assert_eq!(index_text(&root, "stale.txt"), "one\n");
    // An unborn HEAD is a state, not a failure.
    assert!(GitHeadExpectation::Unborn.matches(None));
    assert!(!GitHeadExpectation::Unborn.matches(head));
    assert!(GitHeadExpectation::Uncaptured.matches(head));
}

#[test]
fn line_range_stage_applies_only_the_selected_lines() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "lines.txt", "a\nb\nc\nd\ne\n");
    let changed = "a\nB\nc\nNEW\nd\ne\n";
    std::fs::write(dir.path().join("lines.txt"), changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let policy = plan_policy(&root, "lines.txt", GitDiffLayer::Unstaged);
    let snapshot = read_hunks(&policy).unwrap();
    assert_eq!(snapshot.hunks.len(), 1, "fixture must produce one hunk");
    let hunk = &snapshot.hunks[0];
    let add_index = hunk
        .lines
        .iter()
        .position(|line| line.kind == GitDiffLineKind::Add)
        .expect("fixture must add a line") as u32;
    let mut plan = plan_template(policy.clone(), GitMutationKind::StageLines);
    plan.hunk_id = Some(hunk.id);
    plan.selected = Some(hunk.clone());
    plan.selected_lines = vec![add_index];
    plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
    plan.expected_worktree = snapshot.worktree_sha256;
    let result = run_plan(plan).unwrap();
    assert_eq!(result.kind, GitMutationKind::StageLines);
    assert_eq!(result.hunk, Some(hunk.id));
    // Only the selected addition entered the index; the deletion stayed unstaged.
    assert_eq!(index_text(&root, "lines.txt"), "a\nb\nB\nc\nd\ne\n");
    assert_eq!(
        std::fs::read_to_string(root.join("lines.txt")).unwrap(),
        changed
    );
    let remaining = read_hunks(&policy).unwrap();
    assert!(!remaining.hunks.is_empty(), "the deletion stays unstaged");
}

#[test]
fn invalid_line_selections_are_rejected_without_writing_the_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "range.txt", "a\nb\nc\n");
    std::fs::write(dir.path().join("range.txt"), "a\nB\nc\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let policy = plan_policy(&root, "range.txt", GitDiffLayer::Unstaged);
    let snapshot = read_hunks(&policy).unwrap();
    let hunk = &snapshot.hunks[0];
    let index_before = git_index_fingerprint(&root).unwrap();
    let build = |selected: Vec<u32>| {
        let mut plan = plan_template(policy.clone(), GitMutationKind::StageLines);
        plan.hunk_id = Some(hunk.id);
        plan.selected = Some(hunk.clone());
        plan.selected_lines = selected;
        plan.expected_index = Some(index_before);
        plan.expected_worktree = snapshot.worktree_sha256;
        plan
    };
    assert_eq!(
        run_plan(build(vec![99])),
        Err(GitMutationError::InvalidRange),
        "out-of-range"
    );
    assert_eq!(
        run_plan(build(vec![1, 1])),
        Err(GitMutationError::InvalidRange),
        "duplicate"
    );
    assert_eq!(
        run_plan(build(vec![2, 1])),
        Err(GitMutationError::InvalidRange),
        "unsorted"
    );
    let context = hunk
        .lines
        .iter()
        .position(|line| line.kind == GitDiffLineKind::Context)
        .unwrap() as u32;
    assert_eq!(
        run_plan(build(vec![context])),
        Err(GitMutationError::InvalidRange),
        "context is not a change"
    );
    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
    assert_eq!(index_text(&root, "range.txt"), "a\nb\nc\n");
    // An empty selection never reaches a worker.
    let mut empty = build(Vec::new());
    empty.selected_lines.clear();
    let mut service = GitMutationWorker::default();
    assert_eq!(
        service.start(GitMutationRequest::Plan(empty)),
        Err(GitMutationError::Invalid)
    );
}

#[test]
fn hunk_level_unstage_returns_one_hunk_to_head() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "two-way.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 38\n", "changed 38\n");
    std::fs::write(dir.path().join("two-way.txt"), &changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let unstaged = plan_policy(&root, "two-way.txt", GitDiffLayer::Unstaged);
    let staged = plan_policy(&root, "two-way.txt", GitDiffLayer::Staged);
    assert_eq!(
        read_hunks(&unstaged).unwrap().hunks.len(),
        2,
        "fixture must produce two separated hunks"
    );
    // Stage both hunks through the plan path.
    loop {
        let snapshot = read_hunks(&unstaged).unwrap();
        let Some(hunk) = snapshot.hunks.first().cloned() else {
            break;
        };
        let mut plan = plan_template(unstaged.clone(), GitMutationKind::StageHunk);
        plan.hunk_id = Some(hunk.id);
        plan.selected = Some(hunk);
        plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
        plan.expected_worktree = snapshot.worktree_sha256;
        assert_eq!(run_plan(plan).unwrap().kind, GitMutationKind::StageHunk);
    }
    assert_eq!(index_text(&root, "two-way.txt"), changed);
    let staged_snapshot = read_hunks(&staged).unwrap();
    assert_eq!(staged_snapshot.hunks.len(), 2);
    // Unstage exactly the first staged hunk.
    let target = staged_snapshot.hunks[0].clone();
    let mut plan = plan_template(staged.clone(), GitMutationKind::UnstageHunk);
    plan.hunk_id = Some(target.id);
    plan.selected = Some(target.clone());
    plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
    let result = run_plan(plan).unwrap();
    assert_eq!(result.kind, GitMutationKind::UnstageHunk);
    assert_eq!(result.hunk, Some(target.id));
    assert_ne!(result.before, result.after);
    // An unstage never writes the worktree.
    assert_eq!(
        std::fs::read_to_string(root.join("two-way.txt")).unwrap(),
        changed
    );
    // Exactly the first change left the index; the second is still staged.
    let expected_index = original.replace("line 38\n", "changed 38\n");
    assert_eq!(index_text(&root, "two-way.txt"), expected_index);
    let remaining = read_hunks(&staged).unwrap();
    assert_eq!(remaining.hunks.len(), 1, "the other hunk stays staged");
    // The reverted change is offered again as unstaged.
    let back = read_hunks(&unstaged).unwrap();
    assert_eq!(back.hunks.len(), 1);
    assert!(
        back.hunks[0]
            .lines
            .iter()
            .any(|line| line.text.contains("changed 2")),
        "the reverted change returns to the unstaged layer"
    );
}

#[test]
fn plan_hunk_stage_uses_the_same_capture_as_the_legacy_path() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "plan-hunk.txt", &original);
    let changed = original.replace("line 2\n", "changed 2\n");
    std::fs::write(dir.path().join("plan-hunk.txt"), &changed).unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let policy = plan_policy(&root, "plan-hunk.txt", GitDiffLayer::Unstaged);
    let snapshot = read_hunks(&policy).unwrap();
    assert_eq!(snapshot.hunks.len(), 1);
    let hunk = snapshot.hunks[0].clone();
    let mut plan = plan_template(policy.clone(), GitMutationKind::StageHunk);
    plan.hunk_id = Some(hunk.id);
    plan.selected = Some(hunk.clone());
    plan.expected_index = Some(git_index_fingerprint(&root).unwrap());
    plan.expected_worktree = snapshot.worktree_sha256;
    let result = run_plan(plan).unwrap();
    assert_eq!(result.kind, GitMutationKind::StageHunk);
    assert_eq!(result.hunk, Some(hunk.id));
    assert_eq!(index_text(&root, "plan-hunk.txt"), changed);
    assert_eq!(
        std::fs::read_to_string(root.join("plan-hunk.txt")).unwrap(),
        changed
    );
    assert!(read_hunks(&policy).unwrap().hunks.is_empty());
    assert_eq!(
        read_hunks(&plan_policy(&root, "plan-hunk.txt", GitDiffLayer::Staged))
            .unwrap()
            .hunks
            .len(),
        1
    );
}

#[test]
fn plan_requires_the_captured_hunk_and_rejects_a_foreign_selection() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "foreign.txt", "one\ntwo\nthree\n");
    std::fs::write(dir.path().join("foreign.txt"), "one\nchanged\nthree\n").unwrap();
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let policy = plan_policy(&root, "foreign.txt", GitDiffLayer::Unstaged);
    let snapshot = read_hunks(&policy).unwrap();
    let hunk = snapshot.hunks[0].clone();
    let index_before = git_index_fingerprint(&root).unwrap();

    // A hunk identity that no longer exists is refused.
    let mut vanished = plan_template(policy.clone(), GitMutationKind::StageHunk);
    vanished.hunk_id = Some(GitHunkId(hunk.id.0 ^ 0xffff));
    vanished.selected = Some(hunk.clone());
    vanished.expected_index = Some(index_before);
    vanished.expected_worktree = snapshot.worktree_sha256;
    assert_eq!(run_plan(vanished), Err(GitMutationError::WrongHunk));

    // A selection that does not match the captured revision is refused.
    let mut foreign = hunk.clone();
    foreign.lines.push(GitDiffLine {
        kind: GitDiffLineKind::Add,
        old_line: None,
        new_line: None,
        text: "foreign\n".into(),
    });
    let mut mismatched = plan_template(policy.clone(), GitMutationKind::StageHunk);
    mismatched.hunk_id = Some(hunk.id);
    mismatched.selected = Some(foreign);
    mismatched.expected_index = Some(index_before);
    mismatched.expected_worktree = snapshot.worktree_sha256;
    assert_eq!(run_plan(mismatched), Err(GitMutationError::WrongHunk));

    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
    assert_eq!(index_text(&root, "foreign.txt"), "one\ntwo\nthree\n");

    // A hunk-scoped plan with no captured hunk never reaches a worker.
    let mut missing = plan_template(policy, GitMutationKind::StageHunk);
    missing.expected_index = Some(index_before);
    missing.expected_worktree = snapshot.worktree_sha256;
    let mut service = GitMutationWorker::default();
    assert_eq!(
        service.start(GitMutationRequest::Plan(missing)),
        Err(GitMutationError::Invalid)
    );
    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
}

#[test]
fn unstage_reports_no_change_when_the_index_already_matches_head() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "clean.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let index_before = git_index_fingerprint(&root).unwrap();
    let mut plan = plan_template(
        plan_policy(&root, "clean.txt", GitDiffLayer::Unstaged),
        GitMutationKind::UnstageFile,
    );
    plan.expected_index = Some(index_before);
    assert_eq!(run_plan(plan), Err(GitMutationError::NoChange));
    assert_eq!(git_index_fingerprint(&root).unwrap(), index_before);
    assert_eq!(index_text(&root, "clean.txt"), "one\n");
}

fn run_branch(
    request: crate::git::GitBranchRequest,
) -> Result<crate::git::GitBranchResult, crate::git::GitBranchError> {
    use crate::git::GitBranchWorker;
    let mut service = GitBranchWorker::default();
    service.start(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match service.poll() {
            Ok(Some(result)) => return Ok(result.clone()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        assert!(Instant::now() < deadline, "branch request did not finish");
        thread::yield_now()
    }
}

fn branch_list(root: &Path) -> crate::git::GitBranchList {
    match run_branch(crate::git::GitBranchRequest::List(
        crate::git::GitBranchListRequest {
            repository: root.to_path_buf(),
            deadline: Duration::from_secs(5),
        },
    ))
    .unwrap()
    {
        crate::git::GitBranchResult::List(list) => list,
        other => panic!("expected a list, got {other:?}"),
    }
}

fn create_branch(repository: &Path, name: &str) {
    let repo = git2::Repository::open(repository).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
}

fn branch_request(
    repository: &Path,
    kind: &str,
    name: &str,
    extra: &str,
) -> crate::git::GitBranchRequest {
    use crate::git::GitBranchRequest as R;
    let repository = repository.to_path_buf();
    let deadline = Duration::from_secs(5);
    match kind {
        "worktree" => R::CreateWorktree {
            repository,
            name: name.into(),
            path: PathBuf::from(extra),
            reference: format!("refs/heads/{name}"),
            deadline,
        },
        "remove-worktree" => R::RemoveWorktree {
            repository,
            name: name.into(),
            deadline,
        },
        _ => panic!("unknown request kind {kind}"),
    }
}

#[test]
fn branch_list_reports_the_real_checked_out_worktree_including_a_linked_one() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "b.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    create_branch(&root, "side");
    let linked = root.join("wt-side");
    run_branch(branch_request(
        &root,
        "worktree",
        "side",
        &linked.display().to_string(),
    ))
    .unwrap();

    let list = branch_list(&root);
    let main = list
        .worktrees
        .iter()
        .find(|worktree| worktree.main)
        .expect("the main worktree is listed");
    assert!(
        main.branch.is_some(),
        "the main worktree reports its branch: {main:?}"
    );
    let linked_entry = list
        .worktrees
        .iter()
        .find(|worktree| worktree.name == "side")
        .expect("the linked worktree is listed");
    assert_eq!(
        linked_entry.branch.as_deref(),
        Some("side"),
        "a linked worktree reports its own branch"
    );
    // The branch is attributed to the linked worktree, by that worktree's own path.
    let side = list
        .branches
        .iter()
        .find(|branch| branch.name == "side" && !branch.remote)
        .expect("the local branch is listed");
    // Compared canonically: Git spells Windows paths with forward slashes.
    assert_eq!(
        side.checked_out
            .as_deref()
            .map(|path| crate::files::canonicalize(path).unwrap()),
        Some(crate::files::canonicalize(&linked).unwrap()),
        "the branch is attributed to the linked worktree, not the main one"
    );
    assert!(
        list.branches.iter().any(|branch| branch.head),
        "one branch is HEAD"
    );
}

#[test]
fn worktree_list_reports_detached_heads_without_a_branch() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "tracked.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let linked = root.join("detached");
    run_branch(branch_request(
        &root,
        "worktree",
        "side",
        linked.to_str().unwrap(),
    ))
    .unwrap();
    let oid = repo.head().unwrap().target().unwrap();
    repo.set_head_detached(oid).unwrap();
    git2::Repository::open(&linked)
        .unwrap()
        .set_head_detached(oid)
        .unwrap();
    let list = branch_list(&root);
    assert_eq!(list.worktrees.len(), 2);
    assert!(list.worktrees.iter().all(|entry| entry.branch.is_none()));
    assert!(
        list.branches
            .iter()
            .all(|entry| entry.checked_out.is_none())
    );
}

#[test]
fn worktree_mutations_keep_their_completion_until_polled() {
    let dir = tempdir().unwrap();
    let mut service = GitBranchWorker::default();
    let request = branch_request(dir.path(), "remove-worktree", "missing", "");
    service.start(request.clone()).unwrap();
    service.cancel();
    assert_eq!(service.start(request.clone()), Err(GitBranchError::Busy));
    let expires = Instant::now() + Duration::from_secs(5);
    loop {
        match service.poll() {
            Err(GitBranchError::Cancelled | GitBranchError::NotRepository) => break,
            other => assert!(matches!(other, Ok(None))),
        }
        assert!(Instant::now() < expires);
        thread::yield_now();
    }
    service.start(request).unwrap();
}

#[test]
fn worktree_deadlines_refuse_expired_mutations_and_lists() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "tracked.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let linked = root.join("side");
    run_branch(branch_request(
        &root,
        "worktree",
        "side",
        linked.to_str().unwrap(),
    ))
    .unwrap();
    let requests = [
        GitBranchRequest::List(GitBranchListRequest {
            repository: root.clone(),
            deadline: Duration::from_nanos(1),
        }),
        GitBranchRequest::CreateWorktree {
            repository: root.clone(),
            name: "late".into(),
            path: root.join("late"),
            reference: "late".into(),
            deadline: Duration::from_nanos(1),
        },
        GitBranchRequest::CreateDetachedWorktree {
            repository: root.clone(),
            name: "late".into(),
            path: root.join("late"),
            revision: "HEAD".into(),
            deadline: Duration::from_nanos(1),
        },
        GitBranchRequest::RemoveWorktree {
            repository: root.clone(),
            name: "side".into(),
            deadline: Duration::from_nanos(1),
        },
    ];
    for request in requests {
        assert_eq!(run_branch(request), Err(GitBranchError::Deadline));
    }
    assert!(linked.join("tracked.txt").exists());
    assert!(repo.find_worktree("side").is_ok());
    assert!(!root.join("late").exists());
    assert!(repo.find_branch("late", git2::BranchType::Local).is_err());
}

#[test]
fn worktree_creation_preserves_non_utf8_exclude_patterns_and_read_errors() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "tracked.txt", "one\n");
    let exclude = repo.path().join("info/exclude");
    let original = b"# retained\n/non-utf8-\xff";
    std::fs::write(&exclude, original).unwrap();
    run_branch(branch_request(
        dir.path(),
        "worktree",
        "side",
        ".worktree/side",
    ))
    .unwrap();
    let bytes = std::fs::read(&exclude).unwrap();
    assert!(bytes.starts_with(original));
    assert!(bytes.ends_with(b"\n/.worktree/side/\n"));

    std::fs::remove_file(&exclude).unwrap();
    std::fs::create_dir(&exclude).unwrap();
    std::fs::write(exclude.join("keep"), "keep").unwrap();
    assert!(matches!(
        run_branch(branch_request(
            dir.path(),
            "worktree",
            "other",
            ".worktree/other"
        )),
        Err(GitBranchError::Backend(_))
    ));
    assert_eq!(
        std::fs::read_to_string(exclude.join("keep")).unwrap(),
        "keep"
    );
}

#[test]
fn worktree_creation_and_removal_refuse_before_writing() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let read_head = |repo: &git2::Repository| repo.head().unwrap().target().unwrap();
    let main_head = read_head(&repo);
    let main_index = git_index_fingerprint(&root).unwrap();

    // A non-empty destination is refused and left untouched.
    let occupied = root.join("wt-occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("keep.txt"), "keep\n").unwrap();
    assert_eq!(
        run_branch(branch_request(
            &root,
            "worktree",
            "wt-occupied",
            &occupied.display().to_string()
        )),
        Err(crate::git::GitBranchError::RefusedDestinationNotEmpty(
            occupied.display().to_string()
        ))
    );
    assert_eq!(
        std::fs::read_to_string(occupied.join("keep.txt")).unwrap(),
        "keep\n",
        "a refused creation leaves the destination as it was"
    );

    // A clean round trip works and leaves the main worktree's HEAD and index alone.
    create_branch(&root, "trip");
    let trip = root.join("wt-trip");
    run_branch(branch_request(
        &root,
        "worktree",
        "trip",
        &trip.display().to_string(),
    ))
    .unwrap();
    assert!(trip.join("w.txt").is_file(), "the worktree is checked out");
    assert_eq!(read_head(&repo), main_head, "the main HEAD did not move");
    assert_eq!(git_index_fingerprint(&root).unwrap(), main_index);

    // A branch already checked out elsewhere is refused.
    assert_eq!(
        run_branch(branch_request(
            &root,
            "worktree",
            "trip",
            &root.join("wt-trip2").display().to_string()
        )),
        Err(crate::git::GitBranchError::RefusedBranchAlreadyCheckedOut(
            "trip".into()
        ))
    );

    // A dirty worktree is refused removal and its files are unchanged.
    std::fs::write(trip.join("notes.txt"), "unsaved\n").unwrap();
    assert_eq!(
        run_branch(branch_request(&root, "remove-worktree", "trip", "")),
        Err(crate::git::GitBranchError::RefusedDirtyWorktree(
            "trip".into()
        ))
    );
    assert_eq!(
        std::fs::read_to_string(trip.join("notes.txt")).unwrap(),
        "unsaved\n"
    );

    // Clean removal succeeds, and the main worktree is still untouched.
    std::fs::remove_file(trip.join("notes.txt")).unwrap();
    run_branch(branch_request(&root, "remove-worktree", "trip", "")).unwrap();
    assert_eq!(read_head(&repo), main_head);
    assert_eq!(git_index_fingerprint(&root).unwrap(), main_index);
}

#[test]
fn a_locked_or_paused_worktree_refuses_removal_before_any_write() {
    use crate::git::GitBranchError;
    let dir = tempdir().unwrap();
    let root = dir.path().join("main");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    commit(&repo, "h.txt", "one\n");
    let mut options = git2::WorktreeAddOptions::new();
    let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
    let paused_ref = repo.branch("paused-branch", &head_commit, false).unwrap();
    let paused_ref = paused_ref.into_reference();
    options.reference(Some(&paused_ref));
    let worktree = root.join("wt-paused");
    repo.worktree("paused", &worktree, Some(&options)).unwrap();
    drop(paused_ref);
    let root = crate::files::canonicalize(&root).unwrap();
    let worktree = crate::files::canonicalize(&worktree).unwrap();
    let read_head = |repo: &git2::Repository| repo.head().unwrap().target().unwrap();
    let main_head = read_head(&repo);
    let main_index = git_index_fingerprint(&root).unwrap();

    // A locked worktree is refused, and nothing about it moves.
    let locked = repo.find_worktree("paused").unwrap();
    locked.lock(Some("held by another tool")).unwrap();
    drop(locked);
    assert_eq!(
        run_branch(branch_request(&root, "remove-worktree", "paused", "")),
        Err(GitBranchError::RefusedLockedWorktree("paused".into()))
    );
    assert!(worktree.is_dir(), "a refused removal leaves the directory");
    assert!(
        repo.find_worktree("paused").is_ok(),
        "a refused removal leaves the worktree registered"
    );
    assert_eq!(
        git2::Repository::open(&worktree)
            .unwrap()
            .head()
            .unwrap()
            .shorthand(),
        Some("paused-branch"),
        "the worktree still has its branch checked out"
    );

    // An in-progress operation is refused too. MERGE_HEAD is the marker a real merge leaves
    // behind, so writing it into the worktree's own administrative directory constructs the
    // state the check reads rather than simulating the error.
    let unlocked = repo.find_worktree("paused").unwrap();
    unlocked.unlock().unwrap();
    drop(unlocked);
    let administrative = repo.path().join("worktrees").join("paused");
    assert!(
        administrative.is_dir(),
        "the linked worktree has its own administrative directory"
    );
    std::fs::write(
        administrative.join("MERGE_HEAD"),
        format!("{}\n", main_head),
    )
    .unwrap();
    assert_eq!(
        run_branch(branch_request(&root, "remove-worktree", "paused", "")),
        Err(GitBranchError::RefusedActiveOperation(
            "paused".into(),
            "merge".into()
        ))
    );
    assert!(
        worktree.join("h.txt").is_file(),
        "a refused removal leaves the checkout byte-intact"
    );
    assert!(repo.find_worktree("paused").is_ok());
    assert_eq!(read_head(&repo), main_head, "the main HEAD did not move");
    assert_eq!(git_index_fingerprint(&root).unwrap(), main_index);

    // The same refusal stops the removal that would otherwise succeed.
    std::fs::remove_file(administrative.join("MERGE_HEAD")).unwrap();
    run_branch(branch_request(&root, "remove-worktree", "paused", "")).unwrap();
    assert!(
        !worktree.exists(),
        "a clean worktree is removed with its directory"
    );
}

#[test]
fn the_https_and_ssh_transports_are_present_at_runtime() {
    let version = git2::Version::get();
    assert!(version.https(), "the built libgit2 supports HTTPS");
    assert!(version.ssh(), "the built libgit2 supports SSH");
}

#[test]
fn conflict_provenance_reports_the_three_sides_from_the_index_on_demand() {
    use crate::git::read_conflict_state;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let root = crate::files::canonicalize(&root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();

    // A real conflict: both branches change the same line of the same file.
    std::fs::write(root.join("shared.txt"), "one\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
    }
    let base_tip = repo.head().unwrap().target().unwrap();
    // Ours.
    std::fs::write(root.join("shared.txt"), "ours\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&parent])
            .unwrap();
    }
    let our_tip = repo.head().unwrap().target().unwrap();
    // Theirs, on a branch from the base.
    let base_commit = repo.find_commit(base_tip).unwrap();
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let tree_id = {
            let mut builder = repo.treebuilder(None).unwrap();
            let blob = repo.blob(b"theirs\n").unwrap();
            builder.insert("shared.txt", blob, 0o100644).unwrap();
            builder.write().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();

    // A CLEAN repository reports no paused operation and no conflicts.
    let clean = read_conflict_state(&root).unwrap();
    assert_eq!(clean.operation, None, "a clean repository is not paused");
    assert!(clean.conflicts.is_empty(), "and has no conflicts");
    assert!(!clean.is_paused());
    assert_eq!(
        clean.phase(),
        "no operation in progress; 0 conflicted path(s)"
    );

    // Merge theirs into ours and let it conflict for real.
    {
        let their_commit = repo.find_commit(their_tip).unwrap();
        let their_annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
        repo.merge(&[&their_annotated], None, None).unwrap();
    }
    let state = read_conflict_state(&root).unwrap();
    assert_eq!(
        state.operation.as_deref(),
        Some("merge"),
        "the paused operation is named from the repository's own state"
    );
    assert_eq!(state.conflicts.len(), 1, "one path conflicts");
    let conflict = &state.conflicts[0];
    assert_eq!(conflict.path, std::path::Path::new("shared.txt"));
    // Each side's OID is compared against what the INDEX really holds, read through a fresh
    // handle rather than from the same read that produced it.
    let fresh = git2::Repository::open(&root).unwrap();
    let index = fresh.index().unwrap();
    let ours = index
        .get_path(std::path::Path::new("shared.txt"), 2)
        .expect("stage 2 holds our side");
    let theirs = index
        .get_path(std::path::Path::new("shared.txt"), 3)
        .expect("stage 3 holds their side");
    let base = index
        .get_path(std::path::Path::new("shared.txt"), 1)
        .expect("stage 1 holds the base");
    assert_eq!(conflict.ours.map(|side| side.oid), Some(ours.id));
    assert_eq!(conflict.theirs.map(|side| side.oid), Some(theirs.id));
    assert_eq!(conflict.base.map(|side| side.oid), Some(base.id));
    assert_ne!(ours.id, theirs.id, "the sides really differ");
    assert!(conflict.summary().contains("base "));
    assert!(conflict.summary().contains("ours "));
    assert!(conflict.summary().contains("theirs "));

    // ON DEMAND: resolving the conflict changes what the very next read reports, so the state is
    // not a cached answer.
    {
        // The index handle must be WRITTEN back: staging on a dropped handle would change
        // nothing and the assertion below would pass for the wrong reason.
        let mut staging = fresh.index().unwrap();
        staging
            .add_path(std::path::Path::new("shared.txt"))
            .unwrap();
        staging.write().unwrap();
    }
    let after = read_conflict_state(&root).unwrap();
    assert!(
        after.conflicts.is_empty(),
        "a resolved path is no longer reported as conflicted: {:?}",
        after.conflicts
    );
    assert_eq!(
        after.operation.as_deref(),
        Some("merge"),
        "and the operation is still paused, which the bar must keep showing"
    );
    assert_eq!(after.phase(), "merge: no textual conflicts remain");
    let _ = (our_tip, their_tip);
}

/// Build a repository paused in a real merge conflict, returning the working directory.
fn conflicted_fixture(root: &std::path::Path) -> (git2::Oid, git2::Oid) {
    conflicted_fixture_with_extra(root, None)
}

fn conflicted_fixture_with_extra(root: &Path, extra: Option<&Path>) -> (git2::Oid, git2::Oid) {
    let repo = git2::Repository::init(root).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test User").unwrap();
    config.set_str("user.email", "test@example.test").unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    std::fs::write(root.join("shared.txt"), "base\n").unwrap();
    std::fs::write(root.join("untouched.txt"), "original\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index
            .add_path(std::path::Path::new("untouched.txt"))
            .unwrap();
        if let Some(extra) = extra {
            // Seed native names through the index, including on UTF-8-only filesystems.
            let mut entry = index.get_path(Path::new("untouched.txt"), 0).unwrap();
            entry.path = extra.as_os_str().as_encoded_bytes().to_vec();
            index.add_frombuffer(&entry, b"original\n").unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
    }
    let base = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo
            .treebuilder(Some(&base_commit.tree().unwrap()))
            .unwrap();
        let blob = repo.blob(b"theirs\n").unwrap();
        builder.insert("shared.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    std::fs::write(root.join("shared.txt"), "ours\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let our_tip = repo.head().unwrap().target().unwrap();
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();
    let their_commit = repo.find_commit(their_tip).unwrap();
    let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
    repo.merge(&[&annotated], None, None).unwrap();
    drop(annotated);
    drop(their_commit);
    drop(base_commit);
    // git records where the operation started; abort must return there.
    repo.reference("ORIG_HEAD", our_tip, true, "fixture")
        .unwrap();
    (our_tip, their_tip)
}

#[cfg(unix)]
#[test]
fn conflict_paths_preserve_native_bytes_and_do_not_follow_worktree_symlinks() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let native = Path::new(std::ffi::OsStr::from_bytes(b"bad\xff.txt"));
    conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    // Copy the existing conflict to the native name, using Git's byte-preserving index input.
    let shared = repo
        .index()
        .unwrap()
        .conflicts()
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["update-index", "-z", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        write!(stdin, "0 {}\t", git2::Oid::zero()).unwrap();
        stdin.write_all(native.as_os_str().as_bytes()).unwrap();
        stdin.write_all(b"\0").unwrap();
        for (stage, entry) in [(1, shared.ancestor), (2, shared.our), (3, shared.their)] {
            let entry = entry.unwrap();
            write!(stdin, "{:o} {} {stage}\t", entry.mode, entry.id).unwrap();
            stdin.write_all(native.as_os_str().as_bytes()).unwrap();
            stdin.write_all(b"\0").unwrap();
        }
    }
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    let display_alias = dir.path().join(native.to_string_lossy().as_ref());
    std::fs::write(&display_alias, "unrelated\n").unwrap();
    let state = read_conflict_state(dir.path()).unwrap();
    assert!(state.conflicts.iter().any(|entry| entry.path == native));
    assert_eq!(
        read_side_content(dir.path(), native, GitConflictChoice::Theirs).unwrap(),
        "theirs\n"
    );
    assert!(read_side_content(dir.path(), &display_alias, GitConflictChoice::Theirs).is_err());
    let shared_path = dir.path().join("shared.txt");
    std::fs::remove_file(&shared_path).unwrap();
    symlink(&display_alias, &shared_path).unwrap();
    assert_eq!(
        read_side_content(dir.path(), &shared_path, GitConflictChoice::Ours).unwrap(),
        "ours\n"
    );
    assert_eq!(
        std::fs::read_to_string(display_alias).unwrap(),
        "unrelated\n"
    );
}

#[test]
fn continue_requires_identity_and_preserves_every_merge_parent() {
    let dir = tempdir().unwrap();
    let (ours, theirs) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    let head = repo.find_commit(ours).unwrap();
    let signature = repo.signature().unwrap();
    let third = repo
        .commit(
            None,
            &signature,
            &signature,
            "third",
            &head.tree().unwrap(),
            &[&head],
        )
        .unwrap();
    std::fs::write(
        repo.path().join("MERGE_HEAD"),
        format!("{theirs}\n{third}\n"),
    )
    .unwrap();
    std::fs::write(dir.path().join("shared.txt"), "resolved\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("shared.txt")).unwrap();
    index.write().unwrap();
    repo.config().unwrap().set_str("user.name", "").unwrap();
    repo.config().unwrap().set_str("user.email", "").unwrap();
    let error = continue_operation(dir.path(), "merge").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("configure Git user.name and user.email")
    );
    assert_eq!(repo.head().unwrap().target(), Some(ours));
    assert_eq!(repo.state(), git2::RepositoryState::Merge);
    repo.config()
        .unwrap()
        .set_str("user.name", "Test User")
        .unwrap();
    repo.config()
        .unwrap()
        .set_str("user.email", "test@example.test")
        .unwrap();
    let outcome = continue_operation(dir.path(), "merge").unwrap();
    let merged = repo.find_commit(outcome.commit.unwrap()).unwrap();
    assert_eq!(
        merged.parent_ids().collect::<Vec<_>>(),
        vec![ours, theirs, third]
    );
    assert_eq!(merged.author().name(), Some("Test User"));
    assert!(outcome.cleanup_warning.is_none());
    // Recreate failed cleanup's remaining state; retry must not commit a second time.
    std::fs::write(
        repo.path().join("MERGE_HEAD"),
        format!("{theirs}\n{third}\n"),
    )
    .unwrap();
    std::fs::write(dir.path().join("untouched.txt"), "staged after commit\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("untouched.txt")).unwrap();
    index.write().unwrap();
    let staged_before = git_index_fingerprint(dir.path()).unwrap();
    let retried = continue_operation(dir.path(), "merge again").unwrap();
    assert_eq!(retried.commit, Some(merged.id()));
    assert_eq!(repo.head().unwrap().target(), Some(merged.id()));
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), staged_before);
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
}

#[cfg(unix)]
#[test]
fn continue_reports_committed_revision_when_cleanup_fails_and_retry_only_cleans_up() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let (ours, theirs) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    std::fs::write(dir.path().join("shared.txt"), "resolved\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("shared.txt")).unwrap();
    index.write().unwrap();
    let permissions = std::fs::metadata(repo.path()).unwrap().permissions();
    std::fs::set_permissions(repo.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = continue_operation(dir.path(), "merge");
    std::fs::set_permissions(repo.path(), permissions).unwrap();
    let outcome = result.unwrap();
    let oid = outcome.commit.unwrap();
    assert!(outcome.cleanup_warning.unwrap().contains(&oid.to_string()));
    assert_eq!(repo.head().unwrap().target(), Some(oid));
    assert_eq!(
        repo.find_commit(oid)
            .unwrap()
            .parent_ids()
            .collect::<Vec<_>>(),
        vec![ours, theirs]
    );
    assert_eq!(repo.state(), git2::RepositoryState::Merge);
    let retry = continue_operation(dir.path(), "retry").unwrap();
    assert_eq!(retry.commit, Some(oid));
    assert!(retry.cleanup_warning.is_none());
    assert_eq!(repo.head().unwrap().target(), Some(oid));
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
}

#[test]
fn continue_refuses_while_conflicts_remain_and_then_records_two_parents() {
    use crate::git::GitRecoveryRefusal;
    use crate::git::continue_operation;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let (our_tip, their_tip) = conflicted_fixture(&root);

    // CONTINUE WITH CONFLICTS REMAINING is refused, and HEAD does not move.
    assert_eq!(
        continue_operation(&root, ""),
        Err(GitRecoveryRefusal::ConflictsRemain(1)),
        "a half-resolved tree is never committed as a merge"
    );
    let repo = git2::Repository::open(&root).unwrap();
    assert_eq!(
        repo.head().unwrap().target(),
        Some(our_tip),
        "a refused continue leaves HEAD exactly where it was"
    );
    assert_eq!(
        repo.state(),
        git2::RepositoryState::Merge,
        "and leaves the operation paused"
    );

    // Resolve by staging the chosen content, which is what an explicit stage does.
    {
        std::fs::write(root.join("shared.txt"), "resolved\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
    }
    assert_eq!(
        repo.index().unwrap().conflicts().unwrap().count(),
        0,
        "the conflict is resolved once the path is staged"
    );

    let outcome = continue_operation(&root, "merge theirs").expect("a resolved merge continues");
    let commit = repo
        .find_commit(outcome.commit.expect("a merge commit was written"))
        .unwrap();
    assert_eq!(
        commit.parent_count(),
        2,
        "a merge commit records BOTH parents rather than silently becoming an ordinary commit"
    );
    assert_eq!(
        commit.parent_id(0).unwrap(),
        our_tip,
        "first parent is ours"
    );
    assert_eq!(
        commit.parent_id(1).unwrap(),
        their_tip,
        "second parent is theirs, from the recorded MERGE_HEAD"
    );
    assert_eq!(
        repo.state(),
        git2::RepositoryState::Clean,
        "the operation is finished, not left paused"
    );
    assert_eq!(repo.head().unwrap().target(), Some(commit.id()));
}

#[test]
fn abort_cherry_picks_and_reverts_ignore_missing_or_stale_orig_head() {
    for revert in [false, true] {
        for stale in [false, true] {
            let dir = tempdir().unwrap();
            let (ours, theirs) = conflicted_fixture(dir.path());
            let repo = git2::Repository::open(dir.path()).unwrap();
            repo.cleanup_state().unwrap();
            repo.reset(
                &repo.find_object(ours, None).unwrap(),
                git2::ResetType::Hard,
                None,
            )
            .unwrap();
            let incoming = repo.find_commit(theirs).unwrap();
            if revert {
                repo.revert(&incoming, None).unwrap();
            } else {
                repo.cherrypick(&incoming, None).unwrap();
            }
            assert!(repo.index().unwrap().has_conflicts());
            if stale {
                repo.reference("ORIG_HEAD", theirs, true, "stale fixture")
                    .unwrap();
            } else {
                repo.find_reference("ORIG_HEAD").unwrap().delete().unwrap();
            }
            let branch = repo.head().unwrap().name().unwrap().to_owned();
            let outcome = abort_operation(dir.path()).unwrap();
            assert_eq!(outcome.restored_to, Some(ours));
            assert_eq!(repo.head().unwrap().target(), Some(ours));
            assert_eq!(repo.head().unwrap().name(), Some(branch.as_str()));
            assert_eq!(repo.state(), git2::RepositoryState::Clean);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("shared.txt")).unwrap(),
                "ours\n"
            );
        }
    }
}

#[test]
fn abort_rebase_restores_original_branch_attachment_and_tip() {
    let dir = tempdir().unwrap();
    let (ours, theirs) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    repo.cleanup_state().unwrap();
    repo.reset(
        &repo.find_object(ours, None).unwrap(),
        git2::ResetType::Hard,
        None,
    )
    .unwrap();
    // The replayed commit also changes a nonconflicting path; that staged change is not user work.
    let original = repo.find_commit(ours).unwrap();
    let mut builder = repo.treebuilder(Some(&original.tree().unwrap())).unwrap();
    builder
        .insert(
            "untouched.txt",
            repo.blob(b"our clean change\n").unwrap(),
            0o100644,
        )
        .unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let ours = original
        .amend(Some("HEAD"), None, None, None, None, Some(&tree))
        .unwrap();
    repo.reset(
        &repo.find_object(ours, None).unwrap(),
        git2::ResetType::Hard,
        None,
    )
    .unwrap();
    let branch = repo.head().unwrap().name().unwrap().to_owned();
    let upstream = repo.find_annotated_commit(theirs).unwrap();
    let mut rebase = repo.rebase(None, Some(&upstream), None, None).unwrap();
    rebase.next().unwrap().unwrap();
    assert!(repo.head_detached().unwrap());
    assert!(repo.index().unwrap().has_conflicts());
    assert!(
        repo.status_file(Path::new("untouched.txt"))
            .unwrap()
            .contains(git2::Status::INDEX_MODIFIED)
    );
    drop(rebase);
    let outcome = abort_operation(dir.path()).unwrap();
    assert_eq!(outcome.restored_to, Some(ours));
    assert_eq!(repo.head().unwrap().name(), Some(branch.as_str()));
    assert_eq!(repo.head().unwrap().target(), Some(ours));
    assert_eq!(repo.refname_to_id(&branch).unwrap(), ours);
    assert!(!repo.head_detached().unwrap());
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shared.txt")).unwrap(),
        "ours\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("untouched.txt")).unwrap(),
        "our clean change\n"
    );
}

#[test]
fn abort_returns_to_the_recorded_start_and_refuses_to_discard_work() {
    use crate::git::GitRecoveryRefusal;
    use crate::git::abort_operation;
    use crate::git::read_conflict_state;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let (our_tip, _) = conflicted_fixture(&root);

    // ABORT WITH AN UNCOMMITTED CHANGE is refused, and the change survives.
    // h1.txt is untouched by the merge, so it is the user's own work.
    std::fs::write(root.join("mine.txt"), "my work\n").unwrap();
    let refusal = abort_operation(&root).expect_err("abort must not discard the user's work");
    match &refusal {
        GitRecoveryRefusal::WouldDiscard(paths) => {
            assert!(
                paths.contains("mine.txt"),
                "the refusal names the path it would destroy: {paths}"
            );
            assert!(
                refusal.to_string().contains("never throws work away"),
                "and says why it refused: {refusal}"
            );
        }
        other => panic!("an uncommitted change refuses the abort: {other:?}"),
    }
    let repo = git2::Repository::open(&root).unwrap();
    assert_eq!(
        repo.head().unwrap().target(),
        Some(our_tip),
        "the branch is exactly where it was"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("mine.txt")).unwrap(),
        "my work\n",
        "and the user's bytes are intact"
    );
    assert_eq!(repo.state(), git2::RepositoryState::Merge, "still paused");

    // Once the change is committed away, the abort succeeds and returns to the recorded start.
    std::fs::remove_file(root.join("mine.txt")).unwrap();
    let branch = repo.head().unwrap().name().unwrap().to_owned();
    let outcome = abort_operation(&root).expect("a clean abort returns to the recorded start");
    assert_eq!(outcome.operation, "merge");
    assert_eq!(outcome.restored_to, Some(our_tip));
    let repo = git2::Repository::open(&root).unwrap();
    assert_eq!(repo.head().unwrap().name(), Some(branch.as_str()));
    assert!(!repo.head_detached().unwrap());
    assert_eq!(
        repo.state(),
        git2::RepositoryState::Clean,
        "the operation is gone"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(our_tip),
        "and the branch is back at the revision git recorded for the operation"
    );
    assert!(
        read_conflict_state(&root).unwrap().conflicts.is_empty(),
        "no conflicted path is left behind"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("shared.txt")).unwrap(),
        "ours\n",
        "the working tree is back to the recorded content"
    );
}

#[test]
fn recovery_refuses_when_nothing_is_paused() {
    use crate::git::GitRecoveryRefusal;
    use crate::git::abort_operation;
    use crate::git::continue_operation;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "only.txt", "one\n");
    let original_head = repo.head().unwrap().target();
    assert_eq!(
        continue_operation(dir.path(), ""),
        Err(GitRecoveryRefusal::NotPaused("continue")),
        "there is nothing to continue"
    );
    assert_eq!(repo.head().unwrap().target(), original_head);
    assert_eq!(
        abort_operation(dir.path()),
        Err(GitRecoveryRefusal::NotPaused("abort")),
        "and nothing to abort"
    );
    assert_eq!(repo.head().unwrap().target(), original_head);
}

#[test]
fn the_operation_bar_lists_only_the_actions_valid_in_the_repositorys_real_state() {
    use crate::git::GitConflictChoice;
    use crate::git::GitOperationAction;
    use crate::git::GitOperationBar;
    use crate::git::read_conflict_state;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    conflicted_fixture(&root);

    // With conflicts: resolve is offered, CONTINUE IS NOT, abort is.
    let state = read_conflict_state(&root).unwrap();
    let bar = GitOperationBar::of(&state).expect("a paused operation has a bar");
    assert_eq!(bar.operation, "merge");
    assert!(
        bar.blockers.contains(&"shared.txt".to_owned()),
        "the blockers NAME the conflicted paths: {:?}",
        bar.blockers
    );
    assert!(
        bar.offers(GitOperationAction::Resolve),
        "resolving is valid while conflicts remain"
    );
    assert!(
        !bar.offers(GitOperationAction::Continue),
        "continue is NOT valid while conflicts remain: {:?}",
        bar.actions
    );
    assert!(
        bar.offers(GitOperationAction::Abort),
        "abort stays valid through a pause"
    );
    assert_eq!(bar.phase, "merge: 1 conflicted path(s)");

    // After resolving: the bar still exists, its blockers are gone and continue becomes valid.
    {
        std::fs::write(root.join("shared.txt"), "resolved\n").unwrap();
        let repo = git2::Repository::open(&root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
    }
    let state = read_conflict_state(&root).unwrap();
    let bar = GitOperationBar::of(&state).expect("a conflict-free pause STILL has a bar");
    assert!(
        bar.blockers.is_empty(),
        "the blockers are gone: {:?}",
        bar.blockers
    );
    assert!(
        bar.offers(GitOperationAction::Continue),
        "continue is valid once nothing is conflicted: {:?}",
        bar.actions
    );
    assert!(!bar.offers(GitOperationAction::Resolve));
    assert!(
        bar.phase.contains("no textual conflicts remain"),
        "the phase says the pause is still active: {:?}",
        bar.phase
    );

    // With no operation paused the bar is ABSENT rather than empty-but-present.
    let clean = tempdir().unwrap();
    let clean_repo = git2::Repository::init(clean.path()).unwrap();
    commit(&clean_repo, "only.txt", "one\n");
    let state = read_conflict_state(clean.path()).unwrap();
    assert!(
        GitOperationBar::of(&state).is_none(),
        "no operation means no bar at all, never completion"
    );
    let _ = GitConflictChoice::Ours;
}

#[cfg(unix)]
#[test]
fn conflict_replacement_rejects_symlink_aliases_and_normalizes_relative_paths() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    conflicted_fixture(&root);
    let alias = dir.path().join("alias");
    let destination = root.join("shared.txt");
    symlink(&destination, &alias).unwrap();
    let before = std::fs::read(&destination).unwrap();
    assert!(
        plan_replacement(&root, std::slice::from_ref(&alias), GitConflictChoice::Ours).is_err()
    );
    assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
    assert_eq!(std::fs::read(&destination).unwrap(), before);
    let plan = plan_replacement(
        &root,
        &[PathBuf::from("sub/../shared.txt")],
        GitConflictChoice::Ours,
    )
    .unwrap();
    assert_eq!(plan.files[0].0, destination);
    apply_replacement(&plan).unwrap();
    assert_eq!(std::fs::read_to_string(destination).unwrap(), "ours\n");
}

#[cfg(unix)]
#[test]
fn conflict_replacement_preserves_selected_executable_mode_and_refuses_symlink_sides() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    let path = dir.path().join("shared.txt");
    // Modify the real unmerged index entry, as in executable and symlink conflicts.
    let mut index = repo.index().unwrap();
    let mut ours = index.get_path(Path::new("shared.txt"), 2).unwrap();
    ours.mode = 0o100755;
    index.add(&ours).unwrap();
    index.write().unwrap();
    std::fs::remove_file(&path).unwrap();
    let plan = plan_replacement(
        dir.path(),
        std::slice::from_ref(&path),
        GitConflictChoice::Ours,
    )
    .unwrap();
    assert_eq!(plan.files[0].2, 0o100755);
    apply_replacement(&plan).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ours\n");
    assert_ne!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    let plan = plan_replacement(
        dir.path(),
        std::slice::from_ref(&path),
        GitConflictChoice::Theirs,
    )
    .unwrap();
    apply_replacement(&plan).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    ours.mode = 0o120000;
    index.add(&ours).unwrap();
    index.write().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        plan_replacement(
            dir.path(),
            std::slice::from_ref(&path),
            GitConflictChoice::Ours
        ),
        Err(GitConflictResolveError::NotText(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn conflict_replacement_failure_reports_files_already_written() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("missing/second");
    std::fs::write(&first, "old\n").unwrap();
    let plan = GitReplacementPlan {
        choice: GitConflictChoice::Ours,
        files: vec![
            (first.clone(), "new\n".into(), 0o100644),
            (second.clone(), "second\n".into(), 0o100644),
        ],
    };
    let error = apply_replacement(&plan).unwrap_err();
    assert_eq!(error.written, vec![first.clone()]);
    assert_eq!(error.path, second);
    assert_eq!(std::fs::read(&first).unwrap(), b"new\n");
    assert!(!second.exists());
    let message = error.to_string();
    assert!(message.contains(&first.display().to_string()));
    assert!(message.contains(&second.display().to_string()));
    assert!(message.contains("already replaced"));
}

#[test]
fn whole_file_replacement_previews_cancels_and_applies_exactly_its_file_list() {
    use crate::git::GitConflictChoice;
    use crate::git::apply_replacement;
    use crate::git::git_index_fingerprint;
    use crate::git::plan_replacement;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    conflicted_fixture(&root);
    let conflicted = root.join("shared.txt");
    let other = root.join("untouched.txt");
    std::fs::write(&other, "not part of the conflict\n").unwrap();

    let before_conflicted = std::fs::read(&conflicted).unwrap();
    let before_other = std::fs::read(&other).unwrap();
    let index_before = git_index_fingerprint(&root).unwrap();

    let plan = plan_replacement(
        &root,
        std::slice::from_ref(&conflicted),
        GitConflictChoice::Theirs,
    )
    .expect("a conflicted path plans a replacement");
    // The preview names the file AND the side.
    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.files[0].0, conflicted);
    assert_eq!(
        plan.files[0].1, "theirs\n",
        "the plan carries the side's real blob content"
    );
    let preview = plan.preview();
    assert!(
        preview.contains("shared.txt") && preview.contains("theirs"),
        "the preview names the file and the side: {preview}"
    );

    // NOTHING CHANGES BEFORE CONFIRMATION.
    assert_eq!(
        std::fs::read(&conflicted).unwrap(),
        before_conflicted,
        "the file is byte-identical until the replacement is applied"
    );
    assert_eq!(
        git_index_fingerprint(&root).unwrap(),
        index_before,
        "and the index is untouched"
    );

    // APPLYING writes exactly the listed file and nothing else.
    let written = apply_replacement(&plan).expect("the previewed plan applies");
    assert_eq!(written, vec![conflicted.clone()]);
    assert_eq!(
        std::fs::read(&conflicted).unwrap(),
        b"theirs\n",
        "the applied file holds the chosen side's content"
    );
    assert_eq!(
        std::fs::read(&other).unwrap(),
        before_other,
        "a file outside the list is untouched"
    );

    // A plan that cannot be built refuses rather than writing anything: an unconflicted path.
    assert!(
        plan_replacement(&root, std::slice::from_ref(&other), GitConflictChoice::Ours).is_err(),
        "planning for a path with no conflict is refused"
    );
    assert_eq!(
        std::fs::read(&other).unwrap(),
        before_other,
        "and the refusal wrote nothing"
    );
}

#[test]
fn the_bar_names_every_paused_operation_the_contract_lists() {
    use crate::git::GitOperationAction;
    use crate::git::GitOperationBar;
    use crate::git::read_conflict_state;
    // Each pause is constructed from the marker state GIT ITSELF leaves behind, so the bar is
    // proved against real repository state rather than against a hand-made operation name.
    let cases: [(&str, Option<&str>, &str); 4] = [
        ("MERGE_HEAD", None, "merge"),
        ("CHERRY_PICK_HEAD", None, "cherry-pick"),
        ("REVERT_HEAD", None, "revert"),
        ("rebase-merge", None, "rebase"),
    ];
    for (marker, _extra, expected) in cases {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "only.txt", "one\n");
        let gitdir = repo.path().to_path_buf();
        let tip = repo.head().unwrap().target().unwrap();
        if marker == "rebase-merge" {
            std::fs::create_dir_all(gitdir.join(marker)).unwrap();
            std::fs::write(gitdir.join(marker).join("head-name"), "refs/heads/master\n").unwrap();
        } else {
            std::fs::write(gitdir.join(marker), format!("{tip}\n")).unwrap();
        }
        let observed = git2::Repository::open(dir.path()).unwrap().state();
        let state = read_conflict_state(dir.path()).unwrap();
        assert_eq!(
            state.operation.as_deref(),
            Some(expected),
            "the marker {marker} leaves the repository in {observed:?}, which the model names \
             `{expected}`"
        );
        let bar = GitOperationBar::of(&state).expect("a paused operation shows a bar");
        assert_eq!(bar.operation, expected, "the bar names THAT operation");
        assert!(
            bar.blockers.is_empty(),
            "a pause with no conflicting path reports no blockers: {:?}",
            bar.blockers
        );
        assert!(
            bar.offers(GitOperationAction::Continue) == (expected == "merge"),
            "only merge continuation is supported: {expected}"
        );
        assert!(
            bar.offers(GitOperationAction::Abort),
            "abort stays valid for {expected}"
        );
        assert!(
            !bar.offers(GitOperationAction::Resolve),
            "and resolving is NOT offered when no path is conflicted: {:?}",
            bar.actions
        );
        assert!(
            bar.phase.starts_with(expected),
            "the phase names the operation it belongs to: {:?}",
            bar.phase
        );
    }
}

#[test]
fn a_review_worktree_is_detached_at_the_inspected_revision_and_moves_no_branch() {
    use crate::git::GitBranchRequest as R;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init_opts(
        dir.path(),
        git2::RepositoryInitOptions::new().initial_head("main"),
    )
    .unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let main_head = repo.head().unwrap().target().unwrap();
    let main_branch = repo.head().unwrap().shorthand().unwrap().to_owned();

    // A second commit reachable ONLY from another branch: written with commit(None, ...) and then
    // its own ref moved, because HEAD must not move for a review's revision to exist.
    std::fs::write(root.join("review.txt"), "review\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("review.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("dust", "dust@example.com").unwrap();
    let parent = repo.find_commit(main_head).unwrap();
    let review_oid = repo
        .commit(
            None,
            &signature,
            &signature,
            "the reviewed change",
            &tree,
            &[&parent],
        )
        .unwrap();
    repo.reference("refs/heads/review", review_oid, true, "review branch")
        .unwrap();
    drop(tree);
    drop(index);

    let branches_before: Vec<_> = repo
        .branches(None)
        .unwrap()
        .map(|branch| {
            let (branch, _) = branch.unwrap();
            (
                branch.name().unwrap().unwrap().to_owned(),
                branch.get().target().unwrap(),
            )
        })
        .collect();

    let worktree = root.join("wt-review");
    run_branch(R::CreateDetachedWorktree {
        repository: root.clone(),
        name: "review".into(),
        path: worktree.clone(),
        revision: review_oid.to_string(),
        deadline: Duration::from_secs(5),
    })
    .unwrap();

    // The checkout IS the inspected revision, and it is DETACHED: no branch was created for it.
    let linked = git2::Repository::open(&worktree).unwrap();
    assert!(
        linked.head_detached().unwrap(),
        "a review checkout is detached rather than on a branch"
    );
    assert_eq!(linked.head().unwrap().target().unwrap(), review_oid);
    assert!(
        worktree.join("review.txt").is_file(),
        "the reviewed revision's own content is checked out"
    );
    let branches_after: Vec<_> = repo
        .branches(None)
        .unwrap()
        .map(|branch| {
            let (branch, _) = branch.unwrap();
            (
                branch.name().unwrap().unwrap().to_owned(),
                branch.get().target().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        branches_before, branches_after,
        "looking at a review creates no branch and moves no branch target"
    );

    // And the CURRENT branch is untouched: same branch, same commit.
    let head = repo.head().unwrap();
    assert_eq!(head.shorthand().unwrap(), main_branch);
    assert_eq!(
        head.target().unwrap(),
        main_head,
        "the current branch's HEAD did not move"
    );
}

#[test]
fn the_worktree_list_names_the_main_checkout_from_inside_a_linked_worktree() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let main_branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    run_branch(crate::git::GitBranchRequest::CreateWorktree {
        repository: root.clone(),
        name: "feat".into(),
        path: PathBuf::from(".worktree/feat"),
        reference: "feat".into(),
        deadline: Duration::from_secs(5),
    })
    .unwrap();
    let linked = root.join(".worktree/feat");
    let mut service = GitStatusWorker::default();
    service
        .start(GitStatusPolicy {
            root: linked.clone(),
            root_revision: 1,
            maximum_files: 10,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(5),
        })
        .unwrap();
    assert_eq!(
        wait_status(&mut service).worktree_repository.as_deref(),
        root.file_name().and_then(|name| name.to_str())
    );

    let GitBranchResult::List(list) = run_branch(crate::git::GitBranchRequest::List(
        crate::git::GitBranchListRequest {
            repository: linked.clone(),
            deadline: Duration::from_secs(5),
        },
    ))
    .unwrap() else {
        panic!("expected a list");
    };

    assert_eq!(list.worktrees[0].path, root);
    assert_eq!(
        list.worktrees[0].branch.as_deref(),
        Some(main_branch.as_str())
    );
    assert_eq!(
        crate::files::canonicalize(&list.worktrees[1].path).unwrap(),
        crate::files::canonicalize(&linked).unwrap()
    );
}
#[test]
fn worktree_paths_use_the_checkout_with_separate_git_metadata() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("checkout");
    let metadata = dir.path().join("metadata");
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg("--separate-git-dir")
        .arg(&metadata)
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let repo = git2::Repository::open(&root).unwrap();
    commit(&repo, "tracked.txt", "one\n");
    run_branch(branch_request(&root, "worktree", "side", ".worktree/side")).unwrap();
    let linked = root.join(".worktree/side");
    assert!(linked.join("tracked.txt").is_file());
    assert!(!dir.path().join(".worktree/side").exists());
    assert_eq!(
        branch_list(&root).worktrees[0].path,
        crate::files::canonicalize(&root).unwrap()
    );
    // Git does not record a backlink for --separate-git-dir. Never invent a primary path.
    assert!(
        run_branch(GitBranchRequest::List(GitBranchListRequest {
            repository: linked.clone(),
            deadline: Duration::from_secs(5),
        }))
        .is_err()
    );
    // An absolute destination is usable even when the primary checkout cannot be resolved.
    let absolute = dir.path().join("absolute");
    run_branch(branch_request(
        &linked,
        "worktree",
        "absolute",
        absolute.to_str().unwrap(),
    ))
    .unwrap();
    assert!(absolute.join("tracked.txt").is_file());
    repo.config()
        .unwrap()
        .set_str("core.worktree", root.to_str().unwrap())
        .unwrap();
    assert_eq!(
        branch_list(&linked).worktrees[0].path,
        crate::files::canonicalize(&root).unwrap()
    );
    run_branch(branch_request(
        &linked,
        "worktree",
        "another",
        ".worktree/another",
    ))
    .unwrap();
    assert!(root.join(".worktree/another/tracked.txt").is_file());
}

#[test]
fn a_new_worktree_creates_its_branch_at_head_and_stays_out_of_status() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = crate::files::canonicalize(dir.path()).unwrap();
    let head = repo.head().unwrap().target().unwrap();

    run_branch(crate::git::GitBranchRequest::CreateWorktree {
        repository: root.clone(),
        name: "feat-x".into(),
        path: PathBuf::from(".worktree/feat-x"),
        reference: "feat/x".into(),
        deadline: Duration::from_secs(5),
    })
    .unwrap();

    assert!(root.join(".worktree/feat-x/w.txt").exists());
    let branch = repo.find_branch("feat/x", git2::BranchType::Local).unwrap();
    assert_eq!(branch.get().target(), Some(head));
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true);
    assert!(repo.statuses(Some(&mut options)).unwrap().is_empty());
}

#[test]
fn staging_file_lines_takes_each_added_line_with_the_removed_line_it_replaces() {
    use crate::git::GitDiffLine;
    use crate::git::GitDiffLineKind;
    use crate::git::GitHunk;
    use crate::git::GitHunkId;
    let line = |kind, new_line| GitDiffLine {
        kind,
        old_line: None,
        new_line,
        text: String::new(),
    };
    let hunk = GitHunk {
        old_mode: 0o100644,
        new_mode: 0o100644,
        id: GitHunkId(0),
        header: String::new(),
        old_start: 10,
        old_lines: 2,
        new_start: 10,
        new_lines: 3,
        lines: vec![
            line(GitDiffLineKind::Delete, None),
            line(GitDiffLineKind::Delete, None),
            line(GitDiffLineKind::Add, Some(10)),
            line(GitDiffLineKind::Add, Some(11)),
            line(GitDiffLineKind::Add, Some(12)),
        ],
    };
    assert_eq!(hunk.changes_on_lines(10, 10), vec![0, 2]);
    assert_eq!(hunk.changes_on_lines(11, 11), vec![1, 3]);
    assert_eq!(hunk.changes_on_lines(12, 12), vec![4]);
    assert_eq!(hunk.changes_on_lines(10, 12), vec![0, 1, 2, 3, 4]);
    assert_eq!(hunk.changes_on_lines(13, 14), Vec::<u32>::new());
}

fn captured_plan(
    root: &Path,
    policy: GitHunkPolicy,
    kind: GitMutationKind,
    at: usize,
) -> GitChangePlanRequest {
    let snapshot = read_hunks(&policy).unwrap();
    let hunk = snapshot.hunks[at].clone();
    let mut plan = plan_template(policy, kind);
    plan.hunk_id = Some(hunk.id);
    plan.selected = Some(hunk);
    plan.expected_index = Some(git_index_fingerprint(root).unwrap());
    plan.expected_worktree = snapshot.worktree_sha256;
    plan
}

#[test]
fn non_utf8_hunks_are_refused_before_mutation() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "encoding.txt", "placeholder\n");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let mut builder = repo.treebuilder(Some(&head.tree().unwrap())).unwrap();
    builder
        .insert("encoding.txt", repo.blob(b"caf\xe9\n").unwrap(), 0o100644)
        .unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = git2::Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "encoding", &tree, &[&head])
        .unwrap();
    std::fs::write(dir.path().join("encoding.txt"), "café\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("encoding.txt")).unwrap();
    index.write().unwrap();
    let before = git_index_fingerprint(dir.path()).unwrap();
    assert_eq!(
        read_hunks(&plan_policy(
            dir.path(),
            "encoding.txt",
            GitDiffLayer::Staged
        )),
        Err(GitHunkError::BinaryUnsupported)
    );
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), before);
}

#[test]
fn nested_hunk_mutations_use_repository_paths() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    commit(&repo, "f.txt", "old\n");
    commit(&repo, "sub/f.txt", "old\n");
    for name in ["f.txt", "sub/f.txt"] {
        std::fs::write(dir.path().join(name), "new\n").unwrap();
    }
    let nested = dir.path().join("sub");
    let mut policy = plan_policy(dir.path(), "f.txt", GitDiffLayer::Unstaged);
    policy.scope_root = nested.clone();
    run_plan(captured_plan(
        dir.path(),
        policy.clone(),
        GitMutationKind::StageHunk,
        0,
    ))
    .unwrap();
    assert_eq!(index_text(dir.path(), "f.txt"), "old\n");
    assert_eq!(index_text(dir.path(), "sub/f.txt"), "new\n");
    policy.layer = GitDiffLayer::Staged;
    run_plan(captured_plan(
        dir.path(),
        policy.clone(),
        GitMutationKind::UnstageHunk,
        0,
    ))
    .unwrap();
    policy.layer = GitDiffLayer::Unstaged;
    let snapshot = read_hunks(&policy).unwrap();
    let hunk = snapshot.hunks[0].clone();
    super::mutation::reset_hunk(
        GitMutationRequestId(1),
        &GitResetHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: hunk.id,
            selected: hunk,
            selected_lines: vec![],
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(5),
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(nested.join("f.txt")).unwrap(),
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn whole_file_unstage_clears_conflict_entries_without_changing_worktree() {
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    assert!(repo.index().unwrap().has_conflicts());
    let worktree = std::fs::read(dir.path().join("shared.txt")).unwrap();
    let head_blob = repo
        .head()
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .get_path(Path::new("shared.txt"))
        .unwrap()
        .id();
    let mut plan = plan_template(
        plan_policy(dir.path(), "shared.txt", GitDiffLayer::Staged),
        GitMutationKind::UnstageFile,
    );
    plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
    run_plan(plan).unwrap();
    let index = git2::Repository::open(dir.path()).unwrap().index().unwrap();
    assert!(!index.has_conflicts());
    assert_eq!(
        index.get_path(Path::new("shared.txt"), 0).unwrap().id,
        head_blob
    );
    for stage in 1..=3 {
        assert!(index.get_path(Path::new("shared.txt"), stage).is_none());
    }
    assert_eq!(
        std::fs::read(dir.path().join("shared.txt")).unwrap(),
        worktree
    );
}

#[test]
fn whole_file_unstage_preserves_the_index_when_a_head_subtree_is_missing() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    commit(&repo, "nested/file", "old\n");
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let subtree = tree.get_name("nested").unwrap().id().to_string();
    std::fs::write(dir.path().join("nested/file"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("nested/file")).unwrap();
    index.write().unwrap();
    let mut plan = plan_template(
        plan_policy(dir.path(), "nested/file", GitDiffLayer::Staged),
        GitMutationKind::UnstageFile,
    );
    plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
    let index_path = repo.path().join("index");
    let before = std::fs::read(&index_path).unwrap();
    let object_path = repo
        .path()
        .join("objects")
        .join(&subtree[..2])
        .join(&subtree[2..]);
    drop(tree);
    drop(index);
    drop(repo);
    std::fs::remove_file(object_path).unwrap();

    assert_eq!(run_plan(plan), Err(GitMutationError::Backend));
    assert_eq!(std::fs::read(index_path).unwrap(), before);
    assert_eq!(index_text(dir.path(), "nested/file"), "staged\n");
}

#[test]
fn whole_file_unstage_removes_new_paths_absent_from_head() {
    for path in ["new", "nested/new", "new_directory/new"] {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        commit(&repo, "nested/tracked", "old\n");
        std::fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
        std::fs::write(dir.path().join(path), "new\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let mut plan = plan_template(
            plan_policy(dir.path(), path, GitDiffLayer::Staged),
            GitMutationKind::UnstageFile,
        );
        plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
        run_plan(plan).unwrap();
        index.read(true).unwrap();
        assert!(index.get_path(Path::new(path), 0).is_none());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(path)).unwrap(),
            "new\n"
        );
        assert_eq!(index_text(dir.path(), "nested/tracked"), "old\n");
    }
}

#[test]
fn whole_file_unstage_restores_a_staged_deletion() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "file", "old\n");
    std::fs::remove_file(dir.path().join("file")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("file")).unwrap();
    index.write().unwrap();
    let mut plan = plan_template(
        plan_policy(dir.path(), "file", GitDiffLayer::Staged),
        GitMutationKind::UnstageFile,
    );
    plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
    run_plan(plan).unwrap();
    assert_eq!(index_text(dir.path(), "file"), "old\n");
    assert!(!dir.path().join("file").exists());
}

#[test]
fn index_only_unstage_does_not_read_worktree_content() {
    for directory in [false, true] {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "file", "old\n");
        let path = dir.path().join("file");
        std::fs::write(&path, "new\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        let policy = plan_policy(dir.path(), "file", GitDiffLayer::Staged);
        let hunk_plan = captured_plan(dir.path(), policy.clone(), GitMutationKind::UnstageHunk, 0);
        if directory {
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::File::create(&path)
                .unwrap()
                .set_len(1024 * 1024 * 1024)
                .unwrap();
        }
        assert!(read_hunks(&policy).unwrap().worktree_sha256.is_none());
        run_plan(hunk_plan).unwrap();
        assert_eq!(index_text(dir.path(), "file"), "old\n");
        // Re-stage the blob directly, without consulting the unsupported worktree.
        index.read(true).unwrap();
        let mut entry = index.get_path(Path::new("file"), 0).unwrap();
        entry.id = repo.blob(b"new\n").unwrap();
        index.add(&entry).unwrap();
        index.write().unwrap();
        let mut plan = plan_template(policy, GitMutationKind::UnstageFile);
        plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
        run_plan(plan).unwrap();
        assert_eq!(index_text(dir.path(), "file"), "old\n");
        if directory {
            assert!(path.is_dir());
        } else {
            assert_eq!(path.metadata().unwrap().len(), 1024 * 1024 * 1024);
        }
    }
}

// Windows file names cannot contain `*`.
#[cfg(unix)]
#[test]
fn whole_file_unstage_treats_wildcards_literally() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "*", "old\n");
    commit(&repo, "other.txt", "old\n");
    for name in ["*", "other.txt"] {
        std::fs::write(dir.path().join(name), "new\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let mut plan = plan_template(
        plan_policy(dir.path(), "*", GitDiffLayer::Staged),
        GitMutationKind::UnstageFile,
    );
    plan.expected_index = Some(git_index_fingerprint(dir.path()).unwrap());
    run_plan(plan).unwrap();
    assert_eq!(index_text(dir.path(), "*"), "old\n");
    assert_eq!(index_text(dir.path(), "other.txt"), "new\n");
    assert_eq!(
        read_hunks(&plan_policy(dir.path(), "*", GitDiffLayer::Unstaged))
            .unwrap()
            .hunks
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn hunk_mutations_quote_paths_and_leave_similarly_named_files_unchanged() {
    for name in ["foo ", "foo\t", "foo\"", "foo\\", "café "] {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "foo", "old\n");
        commit(&repo, name, "old\n");
        std::fs::write(dir.path().join("foo"), "new\n").unwrap();
        std::fs::write(dir.path().join(name), "new\n").unwrap();
        let policy = plan_policy(dir.path(), name, GitDiffLayer::Unstaged);
        run_plan(captured_plan(
            dir.path(),
            policy.clone(),
            GitMutationKind::StageHunk,
            0,
        ))
        .unwrap();
        assert_eq!(index_text(dir.path(), name), "new\n");
        assert_eq!(index_text(dir.path(), "foo"), "old\n");
        run_plan(captured_plan(
            dir.path(),
            plan_policy(dir.path(), name, GitDiffLayer::Staged),
            GitMutationKind::UnstageHunk,
            0,
        ))
        .unwrap();
        assert_eq!(index_text(dir.path(), name), "old\n");
        let snapshot = read_hunks(&policy).unwrap();
        let hunk = snapshot.hunks[0].clone();
        crate::git::mutation::reset_hunk(
            GitMutationRequestId(1),
            &GitResetHunkRequest {
                policy,
                hunk_snapshot_revision: 1,
                hunk_id: hunk.id,
                selected: hunk,
                selected_lines: vec![],
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 65536,
                deadline: Duration::from_secs(5),
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(name)).unwrap(),
            "old\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("foo")).unwrap(),
            "new\n"
        );
        assert_eq!(index_text(dir.path(), "foo"), "old\n");
    }
}

#[test]
fn hunk_mutations_preserve_missing_final_newlines() {
    for (old, new) in [("old\n", "new"), ("old", "new\n"), ("old", "new")] {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "eof.txt", old);
        std::fs::write(dir.path().join("eof.txt"), new).unwrap();
        let unstaged = plan_policy(dir.path(), "eof.txt", GitDiffLayer::Unstaged);
        run_plan(captured_plan(
            dir.path(),
            unstaged.clone(),
            GitMutationKind::StageHunk,
            0,
        ))
        .unwrap();
        assert_eq!(index_text(dir.path(), "eof.txt"), new);
        let staged = plan_policy(dir.path(), "eof.txt", GitDiffLayer::Staged);
        run_plan(captured_plan(
            dir.path(),
            staged,
            GitMutationKind::UnstageHunk,
            0,
        ))
        .unwrap();
        assert_eq!(index_text(dir.path(), "eof.txt"), old);
        let snapshot = read_hunks(&unstaged).unwrap();
        let hunk = snapshot.hunks[0].clone();
        crate::git::mutation::reset_hunk(
            GitMutationRequestId(1),
            &GitResetHunkRequest {
                policy: unstaged,
                hunk_snapshot_revision: 1,
                hunk_id: hunk.id,
                selected: hunk,
                selected_lines: vec![],
                expected_index: Some(snapshot.index_fingerprint),
                expected_worktree: snapshot.worktree_sha256,
                maximum_patch_bytes: 65536,
                deadline: Duration::from_secs(5),
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("eof.txt")).unwrap(),
            old
        );
    }
}

#[test]
fn untracked_and_unborn_hunks_preserve_file_creation_and_deletion() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("newdir")).unwrap();
    std::fs::write(dir.path().join("newdir/new.txt"), "new\n").unwrap();
    let policy = plan_policy(dir.path(), "newdir/new.txt", GitDiffLayer::Unstaged);
    run_plan(captured_plan(
        dir.path(),
        policy,
        GitMutationKind::StageHunk,
        0,
    ))
    .unwrap();
    let staged = plan_policy(dir.path(), "newdir/new.txt", GitDiffLayer::Staged);
    run_plan(captured_plan(
        dir.path(),
        staged,
        GitMutationKind::UnstageHunk,
        0,
    ))
    .unwrap();
    assert!(
        git2::Repository::open(dir.path())
            .unwrap()
            .index()
            .unwrap()
            .is_empty()
    );
    commit(&repo, "deleted.txt", "deleted\n");
    std::fs::remove_file(dir.path().join("deleted.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("deleted.txt")).unwrap();
    index.write().unwrap();
    let staged = plan_policy(dir.path(), "deleted.txt", GitDiffLayer::Staged);
    run_plan(captured_plan(
        dir.path(),
        staged,
        GitMutationKind::UnstageHunk,
        0,
    ))
    .unwrap();
    assert_eq!(index_text(dir.path(), "deleted.txt"), "deleted\n");
    assert!(!dir.path().join("deleted.txt").exists());
}

#[test]
fn deleted_worktree_files_can_be_staged_and_reset_by_hunk() {
    for legacy in [false, true] {
        let dir = tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        commit(&repo, "deleted.txt", "one\ntwo\n");
        let file = dir.path().join("deleted.txt");
        std::fs::remove_file(&file).unwrap();
        let policy = plan_policy(dir.path(), "deleted.txt", GitDiffLayer::Unstaged);
        let plan = captured_plan(dir.path(), policy.clone(), GitMutationKind::StageHunk, 0);
        assert!(plan.expected_worktree.is_none());
        assert_eq!(plan.selected.as_ref().unwrap().new_mode, 0);
        // Recreating the path invalidates a captured deletion.
        std::fs::write(&file, "one\ntwo\n").unwrap();
        assert_eq!(run_plan(plan.clone()), Err(GitMutationError::StaleWorktree));
        std::fs::remove_file(&file).unwrap();
        if legacy {
            let hunk = plan.selected.clone().unwrap();
            let request = GitStageHunkRequest {
                policy: policy.clone(),
                hunk_snapshot_revision: 1,
                hunk_id: hunk.id,
                selected: hunk,
                expected_index: plan.expected_index,
                expected_worktree: None,
                maximum_patch_bytes: 65536,
                deadline: Duration::from_secs(5),
            };
            let mut service = GitMutationWorker::default();
            service
                .start(GitMutationRequest::StageHunk(request))
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if service.poll().unwrap().is_some() {
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
        } else {
            run_plan(plan).unwrap();
        }
        assert!(
            git2::Repository::open(dir.path())
                .unwrap()
                .index()
                .unwrap()
                .get_path(Path::new("deleted.txt"), 0)
                .is_none()
        );
        run_plan(captured_plan(
            dir.path(),
            plan_policy(dir.path(), "deleted.txt", GitDiffLayer::Staged),
            GitMutationKind::UnstageHunk,
            0,
        ))
        .unwrap();
        assert!(!file.exists());
        let snapshot = read_hunks(&policy).unwrap();
        let hunk = snapshot.hunks[0].clone();
        let request = GitResetHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: hunk.id,
            selected: hunk,
            selected_lines: vec![],
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: None,
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(5),
        };
        let mut service = GitMutationWorker::default();
        service
            .start(GitMutationRequest::ResetUnstagedHunk(request))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if service.poll().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\n");
        assert_eq!(index_text(dir.path(), "deleted.txt"), "one\ntwo\n");
    }
}

#[test]
fn later_hunks_apply_without_preceding_line_shifts() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "later.txt", &original);
    let changed = original
        .replace("line 2\n", "extra\nline 2\n")
        .replace("line 30\n", "changed\n");
    std::fs::write(dir.path().join("later.txt"), &changed).unwrap();
    let policy = plan_policy(dir.path(), "later.txt", GitDiffLayer::Unstaged);
    run_plan(captured_plan(
        dir.path(),
        policy,
        GitMutationKind::StageHunk,
        1,
    ))
    .unwrap();
    assert_eq!(
        index_text(dir.path(), "later.txt"),
        original.replace("line 30\n", "changed\n")
    );
    // Now include the earlier insertion, so reversing the later hunk uses index coordinates.
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("later.txt")).unwrap();
    index.write().unwrap();
    let staged = plan_policy(dir.path(), "later.txt", GitDiffLayer::Staged);
    run_plan(captured_plan(
        dir.path(),
        staged,
        GitMutationKind::UnstageHunk,
        1,
    ))
    .unwrap();
    assert_eq!(
        index_text(dir.path(), "later.txt"),
        original.replace("line 2\n", "extra\nline 2\n")
    );
}

#[cfg(unix)]
#[test]
fn content_hunks_preserve_executable_modes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "script", "old\n");
    std::fs::set_permissions(
        dir.path().join("script"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit(&repo, "script", "old\n");
    std::fs::write(dir.path().join("script"), "new\n").unwrap();
    let policy = plan_policy(dir.path(), "script", GitDiffLayer::Unstaged);
    run_plan(captured_plan(
        dir.path(),
        policy,
        GitMutationKind::StageHunk,
        0,
    ))
    .unwrap();
    assert_eq!(
        git2::Repository::open(dir.path())
            .unwrap()
            .index()
            .unwrap()
            .get_path(Path::new("script"), 0)
            .unwrap()
            .mode,
        0o100755
    );
    let staged = plan_policy(dir.path(), "script", GitDiffLayer::Staged);
    run_plan(captured_plan(
        dir.path(),
        staged,
        GitMutationKind::UnstageHunk,
        0,
    ))
    .unwrap();
    assert_eq!(
        git2::Repository::open(dir.path())
            .unwrap()
            .index()
            .unwrap()
            .get_path(Path::new("script"), 0)
            .unwrap()
            .mode,
        0o100755
    );
}

#[test]
fn unreadable_worktree_is_not_removed() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "file", "one\n");
    let worktree = dir.path().join("review");
    run_branch(branch_request(
        dir.path(),
        "worktree",
        "review",
        worktree.to_str().unwrap(),
    ))
    .unwrap();
    std::fs::write(worktree.join("file"), "saved edits\n").unwrap();
    std::fs::remove_file(worktree.join(".git")).unwrap();
    assert!(run_branch(branch_request(dir.path(), "remove-worktree", "review", "")).is_err());
    assert_eq!(
        std::fs::read_to_string(worktree.join("file")).unwrap(),
        "saved edits\n"
    );
    assert!(repo.find_worktree("review").is_ok());
}

#[test]
fn nested_worktree_exclusion_does_not_hide_siblings() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "file", "one\n");
    let worktree = dir.path().join("src/review[1]");
    run_branch(branch_request(
        dir.path(),
        "worktree",
        "review",
        worktree.to_str().unwrap(),
    ))
    .unwrap();
    std::fs::write(dir.path().join("src/new.rs"), "new\n").unwrap();
    assert!(!repo.status_should_ignore(Path::new("src/new.rs")).unwrap());
    assert!(
        repo.status_should_ignore(Path::new("src/review[1]/"))
            .unwrap()
    );
}

#[test]
fn conflict_replacement_is_atomic_and_restores_missing_working_copies() {
    use crate::git::conflict::GitConflictChoice;
    use crate::git::conflict::GitReplacementPlan;
    use crate::git::conflict::apply_replacement;
    use crate::git::conflict::plan_replacement;
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    let path = dir.path().join("shared.txt");
    let backup = dir.path().join("original");
    std::fs::hard_link(&path, &backup).unwrap();
    let original = std::fs::read(&backup).unwrap();
    apply_replacement(&GitReplacementPlan {
        choice: GitConflictChoice::Ours,
        files: vec![(path.clone(), "ours\n".into(), 0o100644)],
    })
    .unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    std::fs::remove_file(&path).unwrap();
    let plan = plan_replacement(
        dir.path(),
        std::slice::from_ref(&path),
        GitConflictChoice::Ours,
    )
    .unwrap();
    assert!(!path.exists(), "planning must not recreate the file");
    assert_eq!(apply_replacement(&plan).unwrap(), vec![path.clone()]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ours\n");
    assert_eq!(std::fs::read(&backup).unwrap(), original);
}

#[test]
fn abort_refuses_unrelated_staged_edits_without_changing_index_or_worktree() {
    let dir = tempdir().unwrap();
    let (ours, _) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    std::fs::write(dir.path().join("untouched.txt"), "user-staged work\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("untouched.txt")).unwrap();
    index.write().unwrap();
    let before = git_index_fingerprint(dir.path()).unwrap();
    assert!(
        matches!(abort_operation(dir.path()), Err(GitRecoveryRefusal::WouldDiscard(paths)) if paths.contains("untouched.txt"))
    );
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), before);
    assert_eq!(repo.head().unwrap().target(), Some(ours));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("untouched.txt")).unwrap(),
        "user-staged work\n"
    );
}

#[test]
fn abort_protects_staged_edits_to_incoming_changes_already_on_our_branch() {
    let dir = tempdir().unwrap();
    let (ours, theirs) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    repo.cleanup_state().unwrap();
    let amend = |oid, reference| {
        let commit = repo.find_commit(oid).unwrap();
        let mut builder = repo.treebuilder(Some(&commit.tree().unwrap())).unwrap();
        builder
            .insert(
                "untouched.txt",
                repo.blob(b"already shared\n").unwrap(),
                0o100644,
            )
            .unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        commit
            .amend(reference, None, None, None, None, Some(&tree))
            .unwrap()
    };
    let ours = amend(ours, Some("HEAD"));
    let theirs = amend(theirs, Some("refs/heads/theirs"));
    repo.reset(
        &repo.find_object(ours, None).unwrap(),
        git2::ResetType::Hard,
        None,
    )
    .unwrap();
    repo.merge(&[&repo.find_annotated_commit(theirs).unwrap()], None, None)
        .unwrap();
    repo.reference("ORIG_HEAD", ours, true, "fixture").unwrap();
    assert!(repo.index().unwrap().has_conflicts());
    assert_eq!(
        repo.status_file(Path::new("untouched.txt")).unwrap(),
        git2::Status::CURRENT
    );
    std::fs::write(dir.path().join("untouched.txt"), "user-staged work\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("untouched.txt")).unwrap();
    index.write().unwrap();
    let before = git_index_fingerprint(dir.path()).unwrap();
    assert!(
        matches!(abort_operation(dir.path()), Err(GitRecoveryRefusal::WouldDiscard(paths)) if paths.contains("untouched.txt"))
    );
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), before);
    assert_eq!(repo.head().unwrap().target(), Some(ours));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("untouched.txt")).unwrap(),
        "user-staged work\n"
    );
}

#[test]
fn abort_cherry_pick_of_a_root_commit_restores_the_start() {
    let dir = tempdir().unwrap();
    let (ours, theirs) = conflicted_fixture(dir.path());
    let repo = git2::Repository::open(dir.path()).unwrap();
    repo.cleanup_state().unwrap();
    repo.reset(
        &repo.find_object(ours, None).unwrap(),
        git2::ResetType::Hard,
        None,
    )
    .unwrap();
    let signature = repo.signature().unwrap();
    let tree = repo.find_commit(theirs).unwrap().tree().unwrap();
    let root = repo
        .commit(None, &signature, &signature, "root pick", &tree, &[])
        .unwrap();
    repo.cherrypick(&repo.find_commit(root).unwrap(), None)
        .unwrap();
    assert!(repo.index().unwrap().has_conflicts());
    let outcome = abort_operation(dir.path()).unwrap();
    assert_eq!(outcome.restored_to, Some(ours));
    assert_eq!(repo.head().unwrap().target(), Some(ours));
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shared.txt")).unwrap(),
        "ours\n"
    );
}

#[cfg(unix)]
#[test]
fn abort_protects_non_utf8_tracked_worktree_edits() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempdir().unwrap();
    let native = Path::new(std::ffi::OsStr::from_bytes(b"mine\xff.txt"));
    let (ours, _) = conflicted_fixture_with_extra(dir.path(), Some(native));
    let expected = match std::fs::write(dir.path().join(native), "saved edits\n") {
        Ok(()) => Some(b"saved edits\n".to_vec()),
        Err(error) => {
            // On UTF-8-only filesystems the missing tracked path is an unstaged deletion;
            // abort must protect that change too, not skip the undecodable status entry.
            assert_eq!(error.raw_os_error(), Some(nix::libc::EILSEQ));
            None
        }
    };
    let before = git_index_fingerprint(dir.path()).unwrap();
    assert!(matches!(
        abort_operation(dir.path()),
        Err(GitRecoveryRefusal::WouldDiscard(_))
    ));
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), before);
    assert_eq!(
        git2::Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target(),
        Some(ours)
    );
    assert_eq!(std::fs::read(dir.path().join(native)).ok(), expected);
}

#[test]
fn abort_preserves_unrelated_tracked_worktree_edits() {
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    let path = dir.path().join("untouched.txt");
    std::fs::write(&path, "saved edits\n").unwrap();
    assert!(matches!(
        crate::git::conflict::abort_operation(dir.path()),
        Err(crate::git::conflict::GitRecoveryRefusal::WouldDiscard(_))
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "saved edits\n");
    assert_eq!(
        git2::Repository::open(dir.path()).unwrap().state(),
        git2::RepositoryState::Merge
    );
}

#[test]
fn abort_restores_clean_merge_deletions() {
    let dir = tempdir().unwrap();
    conflicted_fixture(dir.path());
    // Record a real incoming deletion, not an arbitrary user-staged index difference.
    let repo = git2::Repository::open(dir.path()).unwrap();
    let incoming = repo
        .find_commit(repo.refname_to_id("MERGE_HEAD").unwrap())
        .unwrap();
    let mut builder = repo.treebuilder(Some(&incoming.tree().unwrap())).unwrap();
    builder.remove("untouched.txt").unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    let deleted = repo
        .commit(None, &sig, &sig, "incoming deletion", &tree, &[&incoming])
        .unwrap();
    std::fs::write(repo.path().join("MERGE_HEAD"), format!("{deleted}\n")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("untouched.txt")).unwrap();
    index.write().unwrap();
    std::fs::remove_file(dir.path().join("untouched.txt")).unwrap();
    crate::git::conflict::abort_operation(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("untouched.txt")).unwrap(),
        "original\n"
    );
}

#[test]
fn unsupported_continuation_refuses_before_writing() {
    use crate::git::conflict::GitOperationAction;
    use crate::git::conflict::GitOperationBar;
    use crate::git::conflict::continue_operation;
    use crate::git::conflict::read_conflict_state;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "file", "old\n");
    let tip = repo.head().unwrap().target().unwrap();
    std::fs::write(repo.path().join("CHERRY_PICK_HEAD"), format!("{tip}\n")).unwrap();
    let state = read_conflict_state(dir.path()).unwrap();
    assert!(
        !GitOperationBar::of(&state)
            .unwrap()
            .offers(GitOperationAction::Continue)
    );
    let before = git_index_fingerprint(dir.path()).unwrap();
    assert!(
        continue_operation(dir.path(), "")
            .unwrap_err()
            .to_string()
            .contains("not supported")
    );
    assert_eq!(repo.head().unwrap().target(), Some(tip));
    assert_eq!(git_index_fingerprint(dir.path()).unwrap(), before);
}

#[test]
fn status_uses_rename_destinations_and_merges_unsaved_paths_once() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "old.txt", "same\n");
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::rename(dir.path().join("old.txt"), dir.path().join("sub/b.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("old.txt")).unwrap();
    index.add_path(Path::new("sub/b.txt")).unwrap();
    index.write().unwrap();
    let mut service = GitStatusWorker::default();
    service
        .start_with_unsaved(
            GitStatusPolicy {
                root: dir.path().join("sub"),
                root_revision: 1,
                maximum_files: 10,
                maximum_path_bytes: 1024,
                deadline: Duration::from_secs(5),
            },
            vec![
                GitUnsavedDocument::new("a.txt".into(), 1),
                GitUnsavedDocument::new("b.txt".into(), 2),
            ],
        )
        .unwrap();
    let snapshot = wait_status(&mut service);
    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(snapshot.entries[0].relative_path, Path::new("a.txt"));
    assert_eq!(snapshot.entries[1].relative_path, Path::new("b.txt"));
    assert_eq!(
        snapshot.entries[1].original_path.as_deref(),
        Some(Path::new("old.txt"))
    );
    assert_eq!(
        snapshot.entries[1].sources.unsaved_document_revision,
        Some(2)
    );
    assert!(snapshot.entries[1].sources.staged);
}

#[test]
fn partial_reset_validates_selections_and_handles_large_selected_runs() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = "a\n".repeat(30_000);
    let changed = "A\n".repeat(30_000);
    commit(&repo, "partial.txt", &original);
    let file = dir.path().join("partial.txt");
    std::fs::write(&file, &changed).unwrap();
    let mut policy = plan_policy(dir.path(), "partial.txt", GitDiffLayer::Unstaged);
    policy.maximum_lines = 100_000;
    policy.maximum_bytes = 1024 * 1024;
    policy.deadline = Duration::from_secs(5);
    let snapshot = read_hunks(&policy).unwrap();
    assert_eq!(snapshot.hunks.len(), 1);
    let hunk = snapshot.hunks[0].clone();
    let mut request = GitResetHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: hunk.id,
        selected: hunk.clone(),
        selected_lines: vec![],
        expected_index: Some(snapshot.index_fingerprint),
        expected_worktree: snapshot.worktree_sha256,
        maximum_patch_bytes: 1024 * 1024,
        deadline: Duration::from_secs(5),
    };
    for invalid in [vec![0, 0], vec![1, 0], vec![hunk.lines.len() as u32]] {
        request.selected_lines = invalid;
        assert_eq!(
            crate::git::mutation::reset_hunk(
                GitMutationRequestId(1),
                &request,
                &AtomicBool::new(false),
            ),
            Err(GitMutationError::InvalidRange)
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), changed);
        assert_eq!(
            git_index_fingerprint(dir.path()).unwrap(),
            snapshot.index_fingerprint
        );
    }
    request.selected_lines = hunk
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.old_line
                .or(line.new_line)
                .is_some_and(|line| line <= 15_000)
                .then_some(index as u32)
        })
        .collect();
    assert_eq!(request.selected_lines.len(), 30_000);
    crate::git::mutation::reset_hunk(GitMutationRequestId(1), &request, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{}{}", "a\n".repeat(15_000), "A\n".repeat(15_000))
    );
    assert_eq!(index_text(dir.path(), "partial.txt"), original);
}

#[test]
fn partial_reset_preserves_unselected_replacement_lines() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "partial.txt", "a\nb\n");
    std::fs::write(dir.path().join("partial.txt"), "A\nB\n").unwrap();
    let policy = plan_policy(dir.path(), "partial.txt", GitDiffLayer::Unstaged);
    let snapshot = read_hunks(&policy).unwrap();
    let hunk = snapshot.hunks[0].clone();
    let selected_lines = hunk
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            matches!(line.text.as_str(), "a\n" | "A\n").then_some(index as u32)
        })
        .collect();
    crate::git::mutation::reset_hunk(
        GitMutationRequestId(1),
        &GitResetHunkRequest {
            policy,
            hunk_snapshot_revision: 1,
            hunk_id: hunk.id,
            selected: hunk,
            selected_lines,
            expected_index: Some(snapshot.index_fingerprint),
            expected_worktree: snapshot.worktree_sha256,
            maximum_patch_bytes: 65536,
            deadline: Duration::from_secs(5),
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("partial.txt")).unwrap(),
        "a\nB\n"
    );
    assert_eq!(index_text(dir.path(), "partial.txt"), "a\nb\n");
}
