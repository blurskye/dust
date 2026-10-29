use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use super::mutation::{stage_hunk, worktree_hash};
use super::*;
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
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
}
#[test]
fn status_is_bounded_sorted_and_idle() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "tracked.txt", "one");
    std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join("new.txt"), "new").unwrap();
    let mut service = GitStatusService::default();
    service
        .start(GitStatusPolicy {
            root: dir.path().into(),
            root_revision: 3,
            maximum_files: 10,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(2),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
                relative_path: "new.txt".into(),
                status: GitFileStatus::Untracked,
                sources: GitChangeSources {
                    staged: false,
                    unstaged: true,
                    unsaved_document_revision: None,
                },
            },
            GitStatusEntry {
                relative_path: "tracked.txt".into(),
                status: GitFileStatus::Modified,
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
#[test]
fn nested_root_is_scoped_and_ceilings_truncate() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    commit(&repo, "nested/tracked.txt", "one");
    std::fs::write(dir.path().join("sibling.txt"), "outside").unwrap();
    std::fs::write(dir.path().join("nested/tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join("nested/new.txt"), "new").unwrap();
    let mut service = GitStatusService::default();
    service
        .start(GitStatusPolicy {
            root: dir.path().join("nested"),
            root_revision: 4,
            maximum_files: 1,
            maximum_path_bytes: 1024,
            deadline: Duration::from_secs(2),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
    assert_eq!(
        GitStatusService::default()
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
    let workdir = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
    };
    let mut service = GitHunkService::default();
    let generation = service.start(policy.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let snapshot = loop {
        if let Some(snapshot) = service.poll().unwrap() {
            break snapshot.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(snapshot.generation, generation);
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
fn buffer_hunks_compare_exact_unsaved_revision_to_index_without_worktree_mutation() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "live.txt", "one\ntwo\nthree\n");
    let workdir = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
    };
    assert!(read_hunks(&policy).unwrap().hunks.is_empty());
    let mut service = GitHunkService::default();
    service
        .start_buffer(
            policy.clone(),
            GitHunkBufferInput {
                revision: 7,
                bytes: b"one\nthree\n".to_vec(),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
    let workdir = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
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
        deadline: Duration::from_secs(2),
    };
    let mut service = GitMutationService::default();
    let generation = service
        .start(GitMutationRequest::StageHunk(request.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let result = loop {
        if let Some(result) = service.poll().unwrap() {
            break result.clone();
        }
        assert!(Instant::now() < deadline);
        thread::yield_now()
    };
    assert_eq!(result.generation, generation);
    assert_eq!(result.kind, GitMutationKind::StageHunk);
    assert_ne!(result.before, result.after);
    assert_eq!(
        std::fs::read_to_string(root.join("m.txt")).unwrap(),
        changed
    );
    let mut stale = request;
    stale.expected_index = Some(index);
    let current = git_index_fingerprint(&root).unwrap();
    let mut second = GitMutationService::default();
    second.start(GitMutationRequest::StageHunk(stale)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
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
fn wait_mutation_error(service: &mut GitMutationService) -> GitMutationError {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Err(error) = service.poll() {
            return error;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
}

#[test]
fn mutation_service_rejects_stale_worktree_wrong_hunk_and_suppresses_cancelled_late_work() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "negative.txt", "one\ntwo\nthree\n");
    std::fs::write(dir.path().join("negative.txt"), "one\nchanged\nthree\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
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
        deadline: Duration::from_secs(2),
    };

    let mut wrong = base.clone();
    wrong.selected.header.push_str(" stale");
    let mut service = GitMutationService::default();
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
        stage_hunk(GitMutationGeneration(77), &base, &AtomicBool::new(true)),
        Err(GitMutationError::Cancelled)
    );
    service
        .start(GitMutationRequest::StageHunk(stale_worktree))
        .unwrap();
    service.cancel();
    let generation = service.next;
    for _ in 0..256 {
        assert!(service.poll().unwrap().is_none());
        assert_eq!(service.next, generation);
    }
}

#[cfg(unix)]
#[test]
fn mutation_service_rejects_symlink_root_replacement_binary_and_directory_paths() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "valid.txt", "one\ntwo\n");
    std::fs::write(dir.path().join("valid.txt"), "one\nchanged\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
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
        deadline: Duration::from_secs(2),
    };
    let mut service = GitMutationService::default();

    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("outside.txt"), "changed\n").unwrap();
    symlink(outside.path().join("outside.txt"), root.join("linked.txt")).unwrap();
    let mut symlink_policy = policy.clone();
    symlink_policy.file = "linked.txt".into();
    let linked_hash = worktree_hash(&root.join("linked.txt")).unwrap().0;
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
    replacement_policy.scope_root = std::fs::canonicalize(replacement.path()).unwrap();
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
            relative_path: "src/a.rs".into(),
            status: GitFileStatus::Modified,
            sources: GitChangeSources {
                staged: false,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            relative_path: "src/nested/b.rs".into(),
            status: GitFileStatus::Conflicted,
            sources: GitChangeSources {
                staged: true,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            relative_path: "docs/readme.md".into(),
            status: GitFileStatus::Untracked,
            sources: GitChangeSources {
                staged: false,
                unstaged: true,
                unsaved_document_revision: None,
            },
        },
        GitStatusEntry {
            relative_path: "../escape".into(),
            status: GitFileStatus::Conflicted,
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
        generation: GitHunkGeneration(1),
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
fn reset_service_restores_only_selected_hunk_and_preserves_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    commit(&repo, "service-reset.txt", &original);
    let changed = original
        .replace("line 2\n", "changed 2\n")
        .replace("line 18\n", "changed 18\n");
    std::fs::write(dir.path().join("service-reset.txt"), &changed).unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        deadline: Duration::from_secs(2),
    };
    let snapshot = read_hunks(&policy).unwrap();
    let before = git_index_fingerprint(&root).unwrap();
    let hash = worktree_hash(&root.join("service-reset.txt")).unwrap().0;
    let request = GitResetHunkRequest {
        policy,
        hunk_snapshot_revision: 1,
        hunk_id: snapshot.hunks[0].id,
        selected: snapshot.hunks[0].clone(),
        expected_index: Some(before),
        expected_worktree: Some(hash),
        maximum_patch_bytes: 65536,
        deadline: Duration::from_secs(2),
    };
    let mut service = GitMutationService::default();
    service
        .start(GitMutationRequest::ResetUnstagedHunk(request))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
        deadline: Duration::from_secs(2),
    }
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
        deadline: Duration::from_secs(2),
    }
}

fn run_plan(plan: GitChangePlanRequest) -> Result<GitMutationResult, GitMutationError> {
    let mut service = GitMutationService::default();
    service.start(GitMutationRequest::Plan(plan)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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

/// Commit after the first one, chaining the current tip as parent.
fn commit_next(repo: &git2::Repository, path: &str, text: &str) {
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

fn wait_status(service: &mut GitStatusService) -> GitStatusSnapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
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
    commit_next(&repo, "unstaged.txt", "one\n");
    commit_next(&repo, "dirty.txt", "one\n");
    std::fs::write(dir.path().join("staged.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    std::fs::write(dir.path().join("unstaged.txt"), "two\n").unwrap();
    let mut service = GitStatusService::default();
    service
        .start_with_unsaved(
            GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 1,
                maximum_files: 64,
                maximum_path_bytes: 4096,
                deadline: Duration::from_secs(2),
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
    assert!(entry("dirty.txt").sources.contains(GitChangeSource::Unsaved {
        document_revision: 7
    }));
    assert!(!entry("dirty.txt").sources.contains(GitChangeSource::Unstaged));
    assert!(matches!(
        service.start_with_unsaved(
            GitStatusPolicy {
                root: dir.path().into(),
                root_revision: 1,
                maximum_files: 64,
                maximum_path_bytes: 4096,
                deadline: Duration::from_secs(2),
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let unstaged = plan_policy(&root, "src.txt", GitDiffLayer::Unstaged);
    assert_eq!(read_hunks(&unstaged).unwrap().source(), GitChangeSource::Unstaged);
    let staged = plan_policy(&root, "src.txt", GitDiffLayer::Staged);
    assert_eq!(read_hunks(&staged).unwrap().source(), GitChangeSource::Staged);
    let mut service = GitHunkService::default();
    service
        .start_buffer(
            plan_policy(&root, "src.txt", GitDiffLayer::Unstaged),
            GitHunkBufferInput {
                revision: 11,
                bytes: b"one\nbuffer\nthree\n".to_vec(),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let index_before = git_index_fingerprint(&root).unwrap();
    let mut plan = plan_template(
        plan_policy(&root, "guard.txt", GitDiffLayer::Unstaged),
        GitMutationKind::StageFile,
    );
    plan.expected_index = Some(index_before);
    plan.expected_worktree = Some(worktree_hash(&root.join("guard.txt")).unwrap().0);
    plan.unsaved_documents = vec![GitUnsavedDocument::new("guard.txt".into(), 3)];
    assert_eq!(run_plan(plan.clone()), Err(GitMutationError::UnsavedDocument));
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    assert_eq!(std::fs::read_to_string(root.join("whole.txt")).unwrap(), "two\n");
    let mut unstage = plan_template(
        plan_policy(&root, "whole.txt", GitDiffLayer::Unstaged),
        GitMutationKind::UnstageFile,
    );
    unstage.expected_index = Some(git_index_fingerprint(&root).unwrap());
    let unstaged = run_plan(unstage).unwrap();
    assert_eq!(unstaged.kind, GitMutationKind::UnstageFile);
    assert_eq!(index_text(&root, "whole.txt"), "one\n");
    // Unstaging never writes the worktree.
    assert_eq!(std::fs::read_to_string(root.join("whole.txt")).unwrap(), "two\n");
}

#[test]
fn plan_is_rejected_after_an_external_index_or_head_change() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "stale.txt", "one\n");
    commit_next(&repo, "other.txt", "one\n");
    std::fs::write(dir.path().join("stale.txt"), "two\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    assert_eq!(std::fs::read_to_string(root.join("lines.txt")).unwrap(), changed);
    let remaining = read_hunks(&policy).unwrap();
    assert!(!remaining.hunks.is_empty(), "the deletion stays unstaged");
}

#[test]
fn invalid_line_selections_are_rejected_without_writing_the_index() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "range.txt", "a\nb\nc\n");
    std::fs::write(dir.path().join("range.txt"), "a\nB\nc\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    let mut service = GitMutationService::default();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    let mut service = GitMutationService::default();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
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

fn configure_identity(repo: &git2::Repository) {
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Dust Fixture").unwrap();
    config.set_str("user.email", "dust@example.invalid").unwrap();
}

fn commit_request(
    root: &Path,
    message: &str,
    amend: bool,
) -> crate::workspace::git::GitCommitRequest {
    crate::workspace::git::GitCommitRequest {
        repository: root.to_path_buf(),
        message: message.into(),
        amend,
        expected_head: match git_head_id(root).unwrap() {
            Some(id) => GitHeadExpectation::At(id),
            None => GitHeadExpectation::Unborn,
        },
        expected_index: git_index_fingerprint(root).unwrap(),
        deadline: Duration::from_secs(2),
    }
}

fn run_commit(
    request: crate::workspace::git::GitCommitRequest,
) -> Result<crate::workspace::git::GitCommitResult, crate::workspace::git::GitCommitError> {
    let mut service = crate::workspace::git::GitCommitService::default();
    service.start(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match service.poll() {
            Ok(Some(result)) => return Ok(result.clone()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        assert!(Instant::now() < deadline, "commit did not finish");
        thread::yield_now()
    }
}

fn head_commit(repo: &git2::Repository) -> git2::Oid {
    repo.head().unwrap().target().unwrap()
}

#[test]
fn commit_writes_exactly_the_staged_bytes_and_leaves_the_worktree() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "staged.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let path = root.join("staged.txt");
    std::fs::write(&path, "two\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.write().unwrap();
    }
    std::fs::write(&path, "worktree only\n").unwrap();
    let parent = head_commit(&repo);
    let result = run_commit(commit_request(&root, "fixture commit\n", false)).unwrap();
    assert!(!result.amended);
    assert_eq!(result.replaced_message, None);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 1);
    assert_eq!(head.parent_id(0).unwrap(), parent);
    let tree = head.tree().unwrap();
    let blob = repo
        .find_blob(tree.get_path(Path::new("staged.txt")).unwrap().id())
        .unwrap();
    assert_eq!(
        blob.content(),
        b"two\n",
        "the commit records the staged bytes, not the worktree"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "worktree only\n");
    assert_eq!(
        head.message().unwrap(),
        "fixture commit\n",
        "the message is recorded verbatim"
    );
}

#[test]
fn commit_refuses_an_empty_staged_set_and_head_does_not_move() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "clean.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let before = head_commit(&repo);
    assert_eq!(
        run_commit(commit_request(&root, "nothing staged\n", false)),
        Err(crate::workspace::git::GitCommitError::EmptyIndex)
    );
    assert_eq!(head_commit(&repo), before);
}

#[test]
fn commit_refuses_a_stale_index_or_head_capture() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "one.txt", "one\n");
    commit_next(&repo, "two.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let path = root.join("two.txt");
    std::fs::write(&path, "two\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("two.txt")).unwrap();
        index.write().unwrap();
    }
    let request = commit_request(&root, "stale commit\n", false);
    // An external index change invalidates the captured index.
    std::fs::write(root.join("one.txt"), "changed\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("one.txt")).unwrap();
        index.write().unwrap();
    }
    assert_eq!(
        run_commit(request.clone()),
        Err(crate::workspace::git::GitCommitError::StaleIndex)
    );
    // An external commit invalidates the captured HEAD even with a fresh index capture.
    let mut fresh = commit_request(&root, "stale commit\n", false);
    fresh.expected_index = git_index_fingerprint(&root).unwrap();
    let tree = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("External", "external@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "external", &tree, &[&parent])
        .unwrap();
    assert_eq!(
        run_commit(fresh),
        Err(crate::workspace::git::GitCommitError::StaleHead)
    );
}

#[test]
fn amend_replaces_the_tip_and_reports_the_replaced_message() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "amend.txt", "one\n");
    commit_next(&repo, "second.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let tip = head_commit(&repo);
    let parent = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .parent_id(0)
        .unwrap();
    let result = run_commit(commit_request(&root, "reworded\n", true)).unwrap();
    assert!(result.amended);
    assert_eq!(
        result.replaced_message.as_deref(),
        Some("fixture"),
        "the composer is told which message it replaced"
    );
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_ne!(head.id(), tip, "the tip is replaced, not extended");
    assert_eq!(head.parent_id(0).unwrap(), parent, "same parents as before");
    assert_eq!(head.message().unwrap(), "reworded\n");
}

#[test]
fn commit_refuses_a_configured_hook_naming_it() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "hooked.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("hooked.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("hooked.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    let hooks = root.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let before = head_commit(&repo);
    let error = run_commit(commit_request(&root, "hooked\n", false)).unwrap_err();
    assert!(
        matches!(&error, crate::workspace::git::GitCommitError::HookConfigured(path) if path.contains("pre-commit")),
        "the refusal names the hook: {error:?}"
    );
    assert_eq!(head_commit(&repo), before, "HEAD must not move");
}

#[test]
fn commit_refuses_requested_signing_naming_the_setting() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    configure_identity(&repo);
    commit(&repo, "signed.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("signed.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("signed.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    repo.config()
        .unwrap()
        .set_bool("commit.gpgsign", true)
        .unwrap();
    let before = head_commit(&repo);
    let error = run_commit(commit_request(&root, "signed\n", false)).unwrap_err();
    assert!(
        matches!(&error, crate::workspace::git::GitCommitError::SigningRequested(setting) if setting == "commit.gpgsign"),
        "the refusal names the setting: {error:?}"
    );
    assert_eq!(head_commit(&repo), before, "HEAD must not move");
}

#[test]
fn commit_reports_missing_identity_distinctly() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    // The host may supply an identity through a global config, so the fixture forces the
    // local value empty rather than removing it: an empty name is not a usable identity.
    let mut config = repo.config().unwrap();
    config.set_str("user.email", "dust@example.invalid").unwrap();
    config.set_str("user.name", "").unwrap();
    drop(config);
    commit(&repo, "anon.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("anon.txt"), "two\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("anon.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    let error = run_commit(commit_request(&root, "anon\n", false)).unwrap_err();
    assert!(
        matches!(&error, crate::workspace::git::GitCommitError::MissingIdentity(what) if what.contains("user.")),
        "identity failure must name what is absent: {error:?}"
    );
}

fn log_page_request(
    root: &Path,
    filter: Option<&str>,
    offset: usize,
    maximum_commits: usize,
) -> crate::workspace::git::GitLogPageRequest {
    crate::workspace::git::GitLogPageRequest {
        repository: root.to_path_buf(),
        filter: filter.map(str::to_owned),
        offset,
        maximum_commits,
        deadline: Duration::from_secs(5),
    }
}

fn run_log(
    request: crate::workspace::git::GitLogRequest,
) -> Result<crate::workspace::git::GitLogResult, crate::workspace::git::GitLogError> {
    use crate::workspace::git::GitLogService;
    let mut service = GitLogService::default();
    service.start(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match service.poll() {
            Ok(Some(result)) => return Ok(result.clone()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        assert!(Instant::now() < deadline, "git log did not finish");
        thread::yield_now()
    }
}

fn log_page(request: crate::workspace::git::GitLogPageRequest) -> crate::workspace::git::GitLogPage {
    match run_log(crate::workspace::git::GitLogRequest::Page(request)).unwrap() {
        crate::workspace::git::GitLogResult::Page(page) => page,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn log_patch(request: crate::workspace::git::GitLogPatchRequest) -> crate::workspace::git::GitLogPatch {
    match run_log(crate::workspace::git::GitLogRequest::Patch(request)).unwrap() {
        crate::workspace::git::GitLogResult::Patch(patch) => patch,
        other => panic!("expected a patch, got {other:?}"),
    }
}

#[test]
fn history_page_lists_real_commits_in_order_with_their_identities() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "one.txt", "one\n");
    commit_next(&repo, "two.txt", "two\n");
    commit_next(&repo, "three.txt", "three\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 10));
    let subjects = page
        .entries
        .iter()
        .map(|entry| entry.summary.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        subjects,
        vec!["fixture", "fixture", "fixture"],
        "every commit fixture uses the same subject"
    );
    assert_eq!(page.total, 3, "the exact total is stated");
    assert_eq!(page.loaded, 3);
    assert!(page.complete(), "nothing hid, nothing withheld");
    assert!(page.omission_note().is_none());
    // Newest first, and every id resolves to a real commit in the repository.
    for pair in page.entries.windows(2) {
        let newer = repo.find_commit(git2::Oid::from_bytes(&pair[0].id.0).unwrap()).unwrap();
        let older = repo.find_commit(git2::Oid::from_bytes(&pair[1].id.0).unwrap()).unwrap();
        assert!(newer.time().seconds() >= older.time().seconds());
    }
    assert_eq!(
        page.entries[2].author, "Dust",
        "the author travels with the commit"
    );
    assert!(page.entries.iter().all(|entry| !entry.merge));
    assert!(
        page.entries[0].refs.iter().any(|name| name == "master" || name == "main"),
        "the branch tip is decorated: {:?}",
        page.entries[0].refs
    );
}

#[test]
fn history_page_patch_is_the_commits_real_patch() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "first.txt", "one\n");
    commit_next(&repo, "second.txt", "two\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 1));
    let tip = page.entries[0].id;
    // Compute the same patch independently with libgit2 in the test.
    let commit = repo
        .find_commit(git2::Oid::from_bytes(&tip.0).unwrap())
        .unwrap();
    let parent = commit.parent(0).unwrap();
    let diff = repo
        .diff_tree_to_tree(Some(&parent.tree().unwrap()), Some(&commit.tree().unwrap()), None)
        .unwrap();
    let mut expected = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        // libgit2 hands the callback a content line without its origin, and a unified diff
        // carries it, so the independent expectation has to write it back too.
        if matches!(line.origin(), ' ' | '+' | '-') {
            expected.push(line.origin());
        }
        expected.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .unwrap();
    let patch = log_patch(crate::workspace::git::GitLogPatchRequest::Commit {
        repository: root.clone(),
        commit: tip,
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    });
    assert_eq!(patch.text, expected, "the patch is the commit's real patch");
    assert!(patch.text.contains("second.txt"));
    assert!(!patch.truncated);
    assert!(!patch.binary);
}

#[test]
fn merge_commit_is_identified_and_its_combined_patch_covers_both_sides() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "base.txt", "base\n");
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    // Left side.
    std::fs::write(dir.path().join("left.txt"), "left\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("left.txt")).unwrap();
    index.write().unwrap();
    let left_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let left = repo
        .commit(Some("refs/heads/left"), &sig, &sig, "left", &left_tree, &[&base])
        .unwrap();
    // Right side.
    std::fs::write(dir.path().join("right.txt"), "right\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("right.txt")).unwrap();
    index.write().unwrap();
    let right_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let right = repo
        .commit(Some("refs/heads/right"), &sig, &sig, "right", &right_tree, &[&base])
        .unwrap();
    // Merge both sides onto the current branch: a two-parent commit whose tree carries
    // both sides' files.
    let left_commit = repo.find_commit(left).unwrap();
    let right_commit = repo.find_commit(right).unwrap();
    let mut merged_index = repo.index().unwrap();
    merged_index.add_path(Path::new("left.txt")).unwrap();
    merged_index.add_path(Path::new("right.txt")).unwrap();
    merged_index.write().unwrap();
    let merged_tree = repo.find_tree(merged_index.write_tree().unwrap()).unwrap();
    // The same libgit2 rule that blocks an amend also blocks a merge: updating a ref requires
    // the first parent to be the current tip. Write the object, then move the branch.
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    let merge_oid = repo
        .commit(
            None,
            &sig,
            &sig,
            "merge both sides",
            &merged_tree,
            &[&left_commit, &right_commit],
        )
        .unwrap();
    repo.reference(&head_name, merge_oid, true, "merge both sides")
        .unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 1));
    assert_eq!(
        page.entries[0].id.0,
        merge_oid.as_bytes(),
        "the merge is the tip"
    );
    assert!(page.entries[0].merge, "a merge is identified, never flattened");
    assert_eq!(page.entries[0].parents.len(), 2);
    let patch = log_patch(crate::workspace::git::GitLogPatchRequest::Merged {
        repository: root,
        commit: page.entries[0].id,
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    });
    assert!(
        patch.text.contains("left.txt") && patch.text.contains("right.txt"),
        "a combined patch covers both sides:\n{}",
        patch.text
    );
}

#[test]
fn history_page_reports_exact_totals_and_continues_from_an_offset() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    for n in 0..5 {
        let name = format!("file{n}.txt");
        if n == 0 {
            commit(&repo, &name, "x\n");
        } else {
            commit_next(&repo, &name, "x\n");
        }
    }
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let first = log_page(log_page_request(&root, None, 0, 2));
    assert_eq!(first.total, 5, "the exact total is stated for a bounded page");
    assert_eq!(first.loaded, 2);
    assert_eq!(first.remaining, 3);
    assert!(!first.complete());
    assert!(first.omission_note().is_some(), "a withheld page says so");
    let second = log_page(log_page_request(&root, None, first.next_offset, 2));
    assert_eq!(second.loaded, 2);
    assert_eq!(second.remaining, 1);
    assert_ne!(
        first.entries[0].id, second.entries[0].id,
        "the next page continues rather than restarting"
    );
}

#[test]
fn history_filter_marks_omissions_and_never_claims_adjacency() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "keep-a.txt", "x\n");
    commit_next(&repo, "other.txt", "x\n");
    commit_next(&repo, "keep-b.txt", "x\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // `commit_next` uses the same subject, so filter on the author of one commit instead by
    // filtering on a subject that only one commit has is impossible here; instead assert the
    // omission accounting with a filter that matches nothing.
    let hidden = log_page(log_page_request(&root, Some("no such subject"), 0, 10));
    assert_eq!(hidden.total, 3, "the total still counts the whole walk");
    assert_eq!(hidden.loaded, 0);
    assert_eq!(hidden.filtered, 3, "every hidden commit is counted");
    assert!(!hidden.complete());
    assert!(
        hidden
            .omission_note()
            .is_some_and(|note| note.contains("hidden by")),
        "a filtered view states what it hid: {:?}",
        hidden.omission_note()
    );
    let all = log_page(log_page_request(&root, None, 0, 10));
    assert!(all.complete());
    assert_eq!(all.filtered, 0);
}

#[test]
fn history_range_patch_covers_the_real_commit_set() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "base.txt", "base\n");
    let base = repo.head().unwrap().target().unwrap();
    commit_next(&repo, "middle.txt", "middle\n");
    commit_next(&repo, "tip.txt", "tip\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let tip = repo.head().unwrap().target().unwrap();
    let patch = log_patch(crate::workspace::git::GitLogPatchRequest::Range {
        repository: root,
        base: GitHeadId(base.as_bytes().try_into().unwrap()),
        tip: GitHeadId(tip.as_bytes().try_into().unwrap()),
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    });
    assert!(
        patch.text.contains("middle.txt") && patch.text.contains("tip.txt"),
        "the range patch spans every commit in the set:\n{}",
        patch.text
    );
    assert!(
        !patch.text.contains("base.txt"),
        "the base commit is the starting point, not part of the change"
    );
}

#[test]
fn history_filter_keeps_matching_commits_and_never_rewrites_their_ancestry() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let subjects = [
        "beta zero",
        "alpha one",
        "beta two",
        "alpha three",
        "beta four",
        "alpha five",
    ];
    let mut created: Vec<(String, git2::Oid)> = Vec::new();
    for (index, subject) in subjects.iter().enumerate() {
        let name = format!("f{index}.txt");
        std::fs::write(dir.path().join(&name), "x\n").unwrap();
        let mut index_file = repo.index().unwrap();
        index_file.add_path(Path::new(&name)).unwrap();
        index_file.write().unwrap();
        let tree = repo
            .find_tree(index_file.write_tree().unwrap())
            .unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, subject, &tree, &parents)
            .unwrap();
        created.push(((*subject).to_owned(), oid));
    }
    let oid_of = |subject: &str| {
        created
            .iter()
            .find(|(name, _)| name == subject)
            .map(|(_, oid)| *oid)
            .unwrap()
    };
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, Some("alpha"), 0, 10));
    assert_eq!(page.total, 6, "the total still counts the whole walk");
    assert_eq!(page.loaded, 3, "exactly the matching commits survive");
    assert_eq!(page.filtered, 3, "every hidden commit is counted exactly");
    assert!(!page.complete());
    let note = page.omission_note().expect("a filtered view states the omission");
    assert!(
        note.contains("alpha") && note.contains('3'),
        "the note names the filter and the count: {note}"
    );
    let survivors = page
        .entries
        .iter()
        .map(|entry| entry.summary.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        survivors,
        vec!["alpha five", "alpha three", "alpha one"],
        "matching commits survive in the order the walk produced them"
    );
    // The decisive check: a surviving commit's parent is its REAL parent, which the filter hid.
    // If the page had rewritten ancestry to the surviving sequence, this would match the next
    // surviving entry instead.
    assert_eq!(
        page.entries[0].parents.first().map(|id| id.0),
        Some(oid_of("beta four").as_bytes().try_into().unwrap()),
        "the parent reference still points at the real, hidden commit"
    );
    assert_ne!(
        page.entries[0].parents.first().map(|id| id.0),
        Some(page.entries[1].id.0),
        "a hidden commit is never presented as adjacent"
    );
}

#[test]
fn history_lanes_follow_real_ancestry_for_a_linear_history() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "lane-a.txt", "a\n");
    commit_next(&repo, "lane-b.txt", "b\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 10));
    let lanes = crate::workspace::git::history_lanes(&page.entries);
    assert_eq!(lanes.len(), 2);
    // One lane throughout, and the root ends it rather than continuing.
    assert!(lanes.iter().all(|lane| lane == "●"), "{lanes:?}");
}

#[test]
fn history_lanes_mark_ancestry_outside_the_bounded_window_as_continuing() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "win-a.txt", "a\n");
    commit_next(&repo, "win-b.txt", "b\n");
    commit_next(&repo, "win-c.txt", "c\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A window of two: the older row's parent lies outside it.
    let page = log_page(log_page_request(&root, None, 0, 2));
    assert_eq!(page.loaded, 2);
    let lanes = crate::workspace::git::history_lanes(&page.entries);
    assert!(lanes[0].starts_with('●'), "{lanes:?}");
    assert!(
        !lanes[0].contains('\u{2193}'),
        "the tip's parent is inside the window: {lanes:?}"
    );
    assert!(
        lanes[1].starts_with('●') && lanes[1].ends_with('\u{2193}'),
        "ancestry outside the window continues rather than ending: {lanes:?}"
    );
    // A window that reaches the root does not claim continuation.
    let whole = log_page(log_page_request(&root, None, 0, 10));
    let lanes = crate::workspace::git::history_lanes(&whole.entries);
    assert!(
        lanes.iter().all(|lane| !lane.contains('\u{2193}')),
        "a window containing the root does not continue: {lanes:?}"
    );
}

#[test]
fn history_lanes_converge_two_parents_at_a_merge() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "m-base.txt", "base\n");
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let branch = |name: &str, reference: &str| {
        std::fs::write(dir.path().join(name), "side\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some(reference), &sig, &sig, name, &tree, &[&base])
            .unwrap()
    };
    let left = branch("m-left.txt", "refs/heads/mleft");
    let right = branch("m-right.txt", "refs/heads/mright");
    let left = repo.find_commit(left).unwrap();
    let right = repo.find_commit(right).unwrap();
    std::fs::write(dir.path().join("m-left.txt"), "side\n").unwrap();
    std::fs::write(dir.path().join("m-right.txt"), "side\n").unwrap();
    let mut merged = repo.index().unwrap();
    merged.add_path(Path::new("m-left.txt")).unwrap();
    merged.add_path(Path::new("m-right.txt")).unwrap();
    merged.write().unwrap();
    let tree = repo.find_tree(merged.write_tree().unwrap()).unwrap();
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    let merge = repo
        .commit(None, &sig, &sig, "m-merge", &tree, &[&left, &right])
        .unwrap();
    repo.reference(&head_name, merge, true, "m-merge").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 10));
    assert!(page.entries[0].merge);
    let lanes = crate::workspace::git::history_lanes(&page.entries);
    assert!(
        lanes[0].contains('\u{256e}'),
        "the merge opens a second lane: {lanes:?}"
    );
    assert!(
        lanes.iter().skip(1).any(|lane| lane.contains('│')),
        "the opened lane stays occupied: {lanes:?}"
    );
    assert!(
        lanes.last().is_some_and(|lane| lane == "●"),
        "the shared base converges both lanes into one: {lanes:?}"
    );
}

fn run_branch(
    request: crate::workspace::git::GitBranchRequest,
) -> Result<crate::workspace::git::GitBranchResult, crate::workspace::git::GitBranchError> {
    use crate::workspace::git::GitBranchService;
    let mut service = GitBranchService::default();
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

fn branch_list(root: &Path) -> crate::workspace::git::GitBranchList {
    match run_branch(crate::workspace::git::GitBranchRequest::List(
        crate::workspace::git::GitBranchListRequest {
            repository: root.to_path_buf(),
            deadline: Duration::from_secs(5),
        },
    ))
    .unwrap()
    {
        crate::workspace::git::GitBranchResult::List(list) => list,
        other => panic!("expected a list, got {other:?}"),
    }
}

fn branch_request(
    repository: &Path,
    kind: &str,
    name: &str,
    extra: &str,
) -> crate::workspace::git::GitBranchRequest {
    use crate::workspace::git::GitBranchRequest as R;
    let repository = repository.to_path_buf();
    let deadline = Duration::from_secs(5);
    match kind {
        "create" => R::Create {
            repository,
            name: name.into(),
            start: None,
            deadline,
        },
        "delete" => R::Delete {
            repository,
            name: name.into(),
            deadline,
        },
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
    run_branch(branch_request(&root, "create", "side", "")).unwrap();
    let linked = root.join("wt-side");
    run_branch(branch_request(&root, "worktree", "side", &linked.display().to_string())).unwrap();

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
    assert_eq!(
        side.checked_out.as_deref(),
        Some(linked.canonicalize().unwrap().as_path()),
        "the branch is attributed to the linked worktree, not the main one"
    );
    assert!(
        list.branches.iter().any(|branch| branch.head),
        "one branch is HEAD"
    );
}

#[test]
fn branch_deletions_are_refused_without_changing_the_repository() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "d.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // An unmerged branch: its own commit is not an ancestor of the checked-out branch.
    run_branch(branch_request(&root, "create", "unmerged", "")).unwrap();
    repo.set_head("refs/heads/unmerged").unwrap();
    repo.checkout_head(None).unwrap();
    commit_next(&repo, "u.txt", "two\n");
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    repo.set_head("refs/heads/master").ok().or_else(|| repo.set_head("refs/heads/main").ok()).unwrap();
    repo.checkout_head(None).unwrap();
    assert_ne!(head_name, repo.head().unwrap().name().unwrap());
    let head_before = repo.head().unwrap().target().unwrap();
    assert_eq!(
        run_branch(branch_request(&root, "delete", "unmerged", "")),
        Err(crate::workspace::git::GitBranchError::RefusedUnmerged(
            "unmerged".into()
        ))
    );
    assert!(
        repo.find_branch("unmerged", git2::BranchType::Local).is_ok(),
        "a refused delete leaves the branch in place"
    );
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);

    // A branch checked out in a LINKED worktree is refused even though the main worktree
    // has a different branch.
    run_branch(branch_request(&root, "create", "held", "")).unwrap();
    run_branch(branch_request(
        &root,
        "worktree",
        "held",
        &root.join("wt-held").display().to_string(),
    ))
    .unwrap();
    assert_eq!(
        run_branch(branch_request(&root, "delete", "held", "")),
        Err(crate::workspace::git::GitBranchError::RefusedCheckedOut(
            "held".into()
        ))
    );
    assert!(repo.find_branch("held", git2::BranchType::Local).is_ok());
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);
}

#[test]
fn worktree_creation_and_removal_refuse_before_writing() {
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
        Err(crate::workspace::git::GitBranchError::RefusedDestinationNotEmpty(
            occupied.display().to_string()
        ))
    );
    assert_eq!(
        std::fs::read_to_string(occupied.join("keep.txt")).unwrap(),
        "keep\n",
        "a refused creation leaves the destination as it was"
    );

    // A clean round trip works and leaves the main worktree's HEAD and index alone.
    run_branch(branch_request(&root, "create", "trip", "")).unwrap();
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
        Err(crate::workspace::git::GitBranchError::RefusedBranchAlreadyCheckedOut(
            "trip".into()
        ))
    );

    // A dirty worktree is refused removal and its files are unchanged.
    std::fs::write(trip.join("notes.txt"), "unsaved\n").unwrap();
    assert_eq!(
        run_branch(branch_request(&root, "remove-worktree", "trip", "")),
        Err(crate::workspace::git::GitBranchError::RefusedDirtyWorktree(
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
fn tracking_a_remote_branch_sets_its_upstream() {
    let dir = tempdir().unwrap();
    let bare = git2::Repository::init_bare(dir.path().join("origin.git")).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = git2::Repository::init(&work).unwrap();
    commit(&repo, "t.txt", "one\n");
    repo.remote("origin", bare.path().to_str().unwrap()).unwrap();
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .or_else(|_| remote.push(&["refs/heads/main:refs/heads/main"], None))
        .unwrap();
    remote
        .fetch(&["refs/heads/*:refs/remotes/origin/*"], None, None)
        .unwrap();
    drop(remote);
    drop(bare);
    let root = std::fs::canonicalize(&work).unwrap();
    let remote_name = branch_list(&root)
        .branches
        .into_iter()
        .find(|branch| branch.remote)
        .map(|branch| branch.name)
        .expect("a remote branch was fetched");
    run_branch(crate::workspace::git::GitBranchRequest::TrackRemote {
        repository: root.clone(),
        name: "tracking".into(),
        remote_branch: remote_name.clone(),
        deadline: Duration::from_secs(5),
    })
    .unwrap();
    let list = branch_list(&root);
    let created = list
        .branches
        .iter()
        .find(|branch| branch.name == "tracking" && !branch.remote)
        .expect("the tracking branch exists");
    assert_eq!(
        created.upstream.as_deref(),
        Some(remote_name.as_str()),
        "the created branch tracks the remote branch"
    );
}

#[test]
fn a_locked_or_paused_worktree_refuses_removal_before_any_write() {
    use crate::workspace::git::GitBranchError;
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
    let root = std::fs::canonicalize(&root).unwrap();
    let worktree = std::fs::canonicalize(&worktree).unwrap();
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
    std::fs::write(administrative.join("MERGE_HEAD"), format!("{}\n", main_head)).unwrap();
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
    assert!(!worktree.exists(), "a clean worktree is removed with its directory");
}

#[test]
fn the_https_and_ssh_transports_are_present_at_runtime() {
    // The owner approved enabling these transports, and a declaration in Cargo.toml is not
    // evidence that they exist in the built binary. Before that change a fetch stopped at
    // "there is no TLS stream available"; a transport that is really present gets as far as
    // attempting a connection. The probe targets the loopback interface on a port nothing
    // listens on, so it proves the transport without sending anything off the machine.
    let dir = tempdir().unwrap();
    let probe = dir.path().join("probe");
    std::fs::create_dir_all(&probe).unwrap();
    let repo = git2::Repository::init(&probe).unwrap();
    repo.remote("origin", "https://127.0.0.1:1/probe.git")
        .unwrap();
    let mut remote = repo.find_remote("origin").unwrap();
    let message = remote
        .fetch(&["+refs/heads/*:refs/remotes/origin/*"], None, None)
        .expect_err("a fetch against a dead port cannot succeed")
        .message()
        .to_owned();
    assert!(
        !message.contains("no TLS stream") && !message.contains("not supported"),
        "the https transport is present in the built binary: {message}"
    );
    assert!(
        message.contains("connect") || message.contains("refused") || message.contains("resolve"),
        "the failure is a connection-level one, which is only reachable with a working transport: {message}"
    );
    // The ssh transport is present too: an ssh URL is not rejected as an unsupported protocol.
    let ssh_probe = dir.path().join("ssh-probe");
    std::fs::create_dir_all(&ssh_probe).unwrap();
    let ssh_repo = git2::Repository::init(&ssh_probe).unwrap();
    ssh_repo
        .remote("origin", "ssh://git@127.0.0.1:1/probe.git")
        .unwrap();
    let mut ssh_remote = ssh_repo.find_remote("origin").unwrap();
    let ssh_message = ssh_remote
        .fetch(&["+refs/heads/*:refs/remotes/origin/*"], None, None)
        .expect_err("an ssh fetch against a dead port cannot succeed")
        .message()
        .to_owned();
    assert!(
        !ssh_message.contains("not supported") && !ssh_message.contains("unsupported"),
        "the ssh transport is present in the built binary: {ssh_message}"
    );
}

#[test]
fn a_fetch_updates_the_remote_tracking_ref_and_attempts_exactly_once() {
    use crate::workspace::git::{GitRemoteError, GitRemoteEvent, GitRemoteOutcome, GitRemoteService};
    let dir = tempdir().unwrap();
    let bare = git2::Repository::init_bare(dir.path().join("origin.git")).unwrap();
    let bare_path = std::fs::canonicalize(bare.path()).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = git2::Repository::init(&work).unwrap();
    commit(&repo, "f.txt", "one\n");
    repo.remote("origin", bare_path.to_str().unwrap()).unwrap();
    push_master(&repo);
    let branch = repo
        .head()
        .unwrap()
        .shorthand()
        .expect("a branch is checked out")
        .to_owned();
    let ours = repo.head().unwrap().target().unwrap();
    drop(bare);

    // The remote ADVANCES from a second repository that starts from the remote's own history —
    // a push from this repository would update its own tracking ref and hide the advance.
    let second_path = dir.path().join("second");
    std::fs::create_dir_all(&second_path).unwrap();
    let second = git2::Repository::init(&second_path).unwrap();
    second
        .remote("origin", bare_path.to_str().unwrap())
        .unwrap();
    {
        let mut handle = second.find_remote("origin").unwrap();
        handle
            .fetch(
                &["+refs/heads/*:refs/remotes/origin/*".to_owned()],
                None,
                None,
            )
            .unwrap();
    }
    let theirs = second
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .expect("the second repository fetched the remote history")
        .target()
        .unwrap();
    assert_eq!(theirs, ours, "it starts from what the remote really holds");
    second
        .reset(
            &second.find_object(theirs, None).unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
    // The shared `commit` helper writes an UNBORN-HEAD commit, which libgit2 refuses once HEAD
    // already points at a commit, so this one names its parent and moves the branch ref.
    let advance = |repo: &git2::Repository, path: &str, text: &str, branch: &str| {
        let root = repo.workdir().unwrap();
        std::fs::write(root.join(path), text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "fixture", &tree, &[&parent])
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
        oid
    };
    let advanced = advance(&second, "g.txt", "two\n", &branch);
    assert_eq!(second.head().unwrap().target(), Some(advanced));
    {
        let mut handle = second.find_remote("origin").unwrap();
        handle
            .push(&[format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
    }
    drop(second);
    let root = std::fs::canonicalize(&work).unwrap();
    let tracking_before = git2::Repository::open(&root)
        .ok()
        .and_then(|r| {
            r.find_reference(&format!("refs/remotes/origin/{branch}"))
                .ok()
                .and_then(|r| r.target())
        });

    let mut service = GitRemoteService::start(4);
    let generation = service
        .fetch(root.clone(), "origin".into(), Duration::from_secs(10))
        .expect("fetch request accepted");
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        match service.poll() {
            Some(GitRemoteEvent::Fetched {
                generation: got,
                outcome,
            }) => {
                assert_eq!(got, generation, "the outcome belongs to this request");
                break outcome;
            }
            None => {
                assert!(Instant::now() < deadline, "fetch timed out");
                thread::yield_now();
            }
        }
    };
    let GitRemoteOutcome::Fetched { remote, updated } = outcome else {
        panic!("a reachable local remote fetches: {outcome:?}");
    };
    assert_eq!(remote, "origin");
    let fresh = git2::Repository::open(&root).unwrap();
    let tracking_after = fresh
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .expect("the remote-tracking ref exists after the fetch")
        .target();
    assert!(
        updated
            .iter()
            .any(|name| name == &format!("refs/remotes/origin/{branch}")),
        "the fetch reports the ref that actually moved: {updated:?}"
    );
    assert_ne!(
        tracking_after, tracking_before,
        "the tracking ref moved rather than staying where it was"
    );
    assert_eq!(
        tracking_after,
        Some(advanced),
        "and it holds the commit the remote actually gained"
    );

    // ONE ATTEMPT: nothing further is delivered, so nothing is retried behind the caller's back.
    for _ in 0..16 {
        assert!(
            service.poll().is_none(),
            "a completed fetch delivers exactly one outcome and no retry"
        );
    }

    // An unknown outcome is JOURNALED and starts no retry.
    let mut journal = crate::workspace::git::GitRemoteJournal::default();
    assert!(journal.record_unknown("origin", "connection dropped mid-transfer"));
    assert!(
        !journal.record_unknown("origin", "connection dropped mid-transfer"),
        "the same unknown outcome is not journaled twice"
    );
    assert_eq!(journal.entries().len(), 1);
    assert!(
        service.poll().is_none(),
        "recording an unknown outcome starts no attempt of its own"
    );

    // A configured remote whose URL cannot be reached FAILS, with the same single-attempt rule.
    let missing = dir.path().join("not-a-remote.git");
    repo.remote_set_url("origin", missing.to_str().unwrap())
        .unwrap();
    let generation = service
        .fetch(root.clone(), "origin".into(), Duration::from_secs(10))
        .expect("fetch request accepted");
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        match service.poll() {
            Some(GitRemoteEvent::Fetched {
                generation: got,
                outcome,
            }) => {
                assert_eq!(got, generation);
                break outcome;
            }
            None => {
                assert!(Instant::now() < deadline, "fetch timed out");
                thread::yield_now();
            }
        }
    };
    assert!(
        matches!(outcome, GitRemoteOutcome::Failed(_)),
        "a remote that cannot be reached FAILS rather than being retried: {outcome:?}"
    );
    for _ in 0..16 {
        assert!(
            service.poll().is_none(),
            "a failed fetch is not retried automatically"
        );
    }

    // An unknown REMOTE NAME is refused before any attempt is made.
    let generation = service
        .fetch(root.clone(), "absent".into(), Duration::from_secs(10))
        .expect("fetch request accepted");
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        match service.poll() {
            Some(GitRemoteEvent::Fetched {
                generation: got,
                outcome,
            }) => {
                assert_eq!(got, generation);
                break outcome;
            }
            None => {
                assert!(Instant::now() < deadline, "fetch timed out");
                thread::yield_now();
            }
        }
    };
    assert_eq!(
        outcome,
        GitRemoteOutcome::Failed(GitRemoteError::UnknownRemote("absent".into())),
        "a remote that is not configured is refused rather than attempted"
    );
    let _ = dir;
}

fn push_master(repo: &git2::Repository) {
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .or_else(|_| remote.push(&["refs/heads/main:refs/heads/main"], None))
        .unwrap();
}

#[test]
fn the_reviewed_update_fast_forwards_reviews_a_merge_for_divergence_and_refuses_a_stale_review() {
    use crate::workspace::git::{
        GitRemoteEvent, GitRemoteOutcome, GitRemoteRefusal, GitRemoteService,
    };
    let dir = tempdir().unwrap();
    let bare = git2::Repository::init_bare(dir.path().join("origin.git")).unwrap();
    let bare_path = std::fs::canonicalize(bare.path()).unwrap();
    drop(bare);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = git2::Repository::init(&work).unwrap();
    commit(&repo, "f.txt", "one\n");
    repo.remote("origin", bare_path.to_str().unwrap()).unwrap();
    push_master(&repo);
    let root = std::fs::canonicalize(&work).unwrap();
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    let ours = repo.head().unwrap().target().unwrap();

    // A helper that advances the REMOTE from a second repository, so the local tracking ref is
    // genuinely behind rather than already up to date.
    let advance_remote = |label: &str| -> git2::Oid {
        let path = dir.path().join(format!("second-{label}"));
        std::fs::create_dir_all(&path).unwrap();
        let second = git2::Repository::init(&path).unwrap();
        second
            .remote("origin", bare_path.to_str().unwrap())
            .unwrap();
        {
            let mut handle = second.find_remote("origin").unwrap();
            handle
                .fetch(&["+refs/heads/*:refs/remotes/origin/*".to_owned()], None, None)
                .unwrap();
        }
        let tip = second
            .find_reference(&format!("refs/remotes/origin/{branch}"))
            .unwrap()
            .target()
            .unwrap();
        second
            .reset(
                &second.find_object(tip, None).unwrap(),
                git2::ResetType::Hard,
                None,
            )
            .unwrap();
        let root = second.workdir().unwrap().to_path_buf();
        std::fs::write(root.join(format!("{label}.txt")), format!("{label}\n")).unwrap();
        let mut index = second.index().unwrap();
        index.add_path(std::path::Path::new(&format!("{label}.txt"))).unwrap();
        index.write().unwrap();
        let tree = second.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = second.head().unwrap().peel_to_commit().unwrap();
        let oid = second
            .commit(None, &sig, &sig, label, &tree, &[&parent])
            .unwrap();
        second
            .reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
        {
            let mut handle = second.find_remote("origin").unwrap();
            handle
                .push(&[format!("refs/heads/{branch}:refs/heads/{branch}")], None)
                .unwrap();
        }
        oid
    };

    let mut service = GitRemoteService::start(4);
    let drain = |service: &mut GitRemoteService, generation| -> GitRemoteOutcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match service.poll() {
                Some(GitRemoteEvent::Fetched { generation: got, outcome }) => {
                    assert_eq!(got, generation);
                    return outcome;
                }
                None => {
                    assert!(Instant::now() < deadline, "remote operation timed out");
                    thread::yield_now();
                }
            }
        }
    };

    // ---- BEHIND: the review says fast-forward, and applying moves the branch to the remote tip.
    let ahead_one = advance_remote("first");
    let generation = service
        .fetch(root.clone(), "origin".into(), Duration::from_secs(10))
        .unwrap();
    let _ = drain(&mut service, generation);
    let generation = service
        .review_update(
            root.clone(),
            "origin".into(),
            branch.clone(),
            Duration::from_secs(10),
        )
        .unwrap();
    let review = match drain(&mut service, generation) {
        GitRemoteOutcome::Reviewed(review) => review,
        other => panic!("a behind branch reviews as a fast-forward: {other:?}"),
    };
    assert_eq!(review.local, ours, "the review starts where the branch is");
    assert_eq!(review.remote_tip, ahead_one, "and targets the remote tip");
    assert_eq!(review.commits, 1, "and counts what would come in");
    let generation = service
        .apply_update(review.clone(), Duration::from_secs(10))
        .unwrap();
    let outcome = drain(&mut service, generation);
    assert_eq!(
        outcome,
        GitRemoteOutcome::Updated {
            branch: branch.clone(),
            from: ours,
            to: ahead_one,
            commits: 1,
        }
    );
    let moved = git2::Repository::open(&root)
        .unwrap()
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .target();
    assert_eq!(
        moved,
        Some(ahead_one),
        "the branch ref moved to the commit the remote really holds"
    );

    // ---- DIVERGED: a local commit the remote does not have. The update is refused BY NAME and
    // the branch is left exactly where it was.
    {
        let repo = git2::Repository::open(&root).unwrap();
        std::fs::write(root.join("local.txt"), "local\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("local.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "local only", &tree, &[&parent])
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
    }
    let diverged_tip = git2::Repository::open(&root)
        .unwrap()
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .target()
        .unwrap();
    let generation = service
        .review_update(
            root.clone(),
            "origin".into(),
            branch.clone(),
            Duration::from_secs(10),
        )
        .unwrap();
    let outcome = drain(&mut service, generation);
    // DIVERGENCE IS NOW A REVIEWED MERGE, not a refusal: the resolver and the recovery actions
    // exist, which is the condition the contract set for enabling this. The review names the
    // integration and moves nothing.
    match outcome {
        GitRemoteOutcome::Reviewed(review) => {
            assert_eq!(
                review.integration,
                crate::workspace::git::GitUpdateIntegration::Merge,
                "a diverged branch reviews as a MERGE"
            );
            assert_eq!(review.local, diverged_tip, "from where the branch is");
            assert_eq!(review.remote_tip, moved.unwrap(), "towards the remote tip");
            assert!(
                review.summary().contains("MERGE"),
                "and the summary says so: {}",
                review.summary()
            );
        }
        other => panic!("a diverged branch reviews as a merge plan: {other:?}"),
    }
    let after_divergence = git2::Repository::open(&root)
        .unwrap()
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .target();
    assert_eq!(
        after_divergence,
        Some(diverged_tip),
        "reviewing a merge moves nothing"
    );

    // ---- STALE: review a real fast-forward, then move the branch before applying. The plan is
    // refused rather than applied to a starting point the user never reviewed.
    {
        let repo = git2::Repository::open(&root).unwrap();
        repo.reference(&format!("refs/heads/{branch}"), ahead_one, true, "fixture")
            .unwrap();
        let mut options = git2::build::CheckoutBuilder::new();
        options.force();
        repo.checkout_head(Some(&mut options)).unwrap();
    }
    let ahead_two = advance_remote("second");
    let generation = service
        .fetch(root.clone(), "origin".into(), Duration::from_secs(10))
        .unwrap();
    let _ = drain(&mut service, generation);
    let generation = service
        .review_update(
            root.clone(),
            "origin".into(),
            branch.clone(),
            Duration::from_secs(10),
        )
        .unwrap();
    let review = match drain(&mut service, generation) {
        GitRemoteOutcome::Reviewed(review) => review,
        other => panic!("a behind branch reviews: {other:?}"),
    };
    assert_eq!(review.remote_tip, ahead_two);
    // The branch moves AFTER the review was captured.
    {
        let repo = git2::Repository::open(&root).unwrap();
        std::fs::write(root.join("late.txt"), "late\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("late.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "moved after review", &tree, &[&parent])
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
    }
    let late_tip = git2::Repository::open(&root)
        .unwrap()
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .target()
        .unwrap();
    let reviewed_local = review.local;
    let generation = service
        .apply_update(review, Duration::from_secs(10))
        .unwrap();
    let outcome = drain(&mut service, generation);
    match outcome {
        GitRemoteOutcome::Refused(GitRemoteRefusal::Stale {
            expected, actual, ..
        }) => {
            assert_eq!(
                expected, reviewed_local,
                "the refusal reports the LOCAL starting point the review was built from"
            );
            assert_ne!(expected, late_tip, "which is not where the branch is now");
            assert_eq!(actual, late_tip, "and it reports where the branch actually is");
        }
        other => panic!("a moved branch refuses the reviewed plan: {other:?}"),
    }
    let after_stale = git2::Repository::open(&root)
        .unwrap()
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .target();
    assert_eq!(
        after_stale,
        Some(late_tip),
        "a stale apply moves nothing at all"
    );
    let _ = dir;
}

#[test]
fn the_push_refspec_is_never_forced() {
    use crate::workspace::git::{push_refspec, refspec_forces};
    let refspec = push_refspec("refs/heads/feature", "refs/heads/target");
    assert_eq!(refspec, "refs/heads/feature:refs/heads/target");
    assert!(
        !refspec_forces(&refspec),
        "the push refspec carries no force marker: {refspec}"
    );
    // The predicate would catch a forced refspec, so the assertion above can fail.
    assert!(refspec_forces("+refs/heads/feature:refs/heads/target"));
    assert!(refspec_forces("  +refs/heads/x:refs/heads/y"));
}

#[test]
fn a_push_is_non_forced_leased_and_names_its_destination() {
    use crate::workspace::git::{GitRemoteEvent, GitRemoteOutcome, GitRemoteRefusal, GitRemoteService};
    let dir = tempdir().unwrap();
    let bare = git2::Repository::init_bare(dir.path().join("origin.git")).unwrap();
    let bare_path = std::fs::canonicalize(bare.path()).unwrap();
    drop(bare);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = git2::Repository::init(&work).unwrap();
    commit(&repo, "f.txt", "one\n");
    repo.remote("origin", bare_path.to_str().unwrap()).unwrap();
    push_master(&repo);
    let root = std::fs::canonicalize(&work).unwrap();
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    let ours = repo.head().unwrap().target().unwrap();

    // Read the value out of the BARE repository itself, through a fresh handle each time.
    let read_remote = |name: &str| -> Option<git2::Oid> {
        let repo = git2::Repository::open(&bare_path).ok()?;
        repo.find_reference(name).ok()?.target()
    };

    let mut service = GitRemoteService::start(4);
    let drain = |service: &mut GitRemoteService| -> GitRemoteOutcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match service.poll() {
                Some(GitRemoteEvent::Fetched { outcome, .. }) => return outcome,
                None => {
                    assert!(Instant::now() < deadline, "push timed out");
                    thread::yield_now();
                }
            }
        }
    };

    // ---- A successful push to an EXPLICIT destination that differs from the branch name.
    let _generation = service
        .push(
            root.clone(),
            "origin".into(),
            format!("refs/heads/{branch}"),
            "refs/heads/reviewed".into(),
            None,
            Duration::from_secs(10),
        )
        .unwrap();
    let outcome = drain(&mut service);
    match outcome {
        GitRemoteOutcome::Pushed {
            destination, to, from, ..
        } => {
            assert_eq!(destination, "refs/heads/reviewed");
            assert_eq!(to, ours, "the pushed commit is the local tip");
            assert_eq!(from, None, "the destination did not exist before");
        }
        other => panic!("a push to a named destination succeeds: {other:?}"),
    }
    assert_eq!(
        read_remote("refs/heads/reviewed"),
        Some(ours),
        "the BARE repository holds the pushed commit at the named destination"
    );

    // ---- Nothing to send: the destination already holds the local tip.
    let _generation = service
        .push(
            root.clone(),
            "origin".into(),
            format!("refs/heads/{branch}"),
            "refs/heads/reviewed".into(),
            None,
            Duration::from_secs(10),
        )
        .unwrap();
    match drain(&mut service) {
        GitRemoteOutcome::Refused(GitRemoteRefusal::UpToDate { branch: named }) => {
            assert_eq!(named, "refs/heads/reviewed");
        }
        other => panic!("pushing nothing reports being up to date: {other:?}"),
    }

    // ---- THE LEASE: the remote advances, and a push based on the older value is refused with
    // the remote left byte-identical.
    let observed = read_remote("refs/heads/reviewed").unwrap();
    // A second repository advances the remote from its own history.
    let second_path = dir.path().join("second");
    std::fs::create_dir_all(&second_path).unwrap();
    let second = git2::Repository::init(&second_path).unwrap();
    second
        .remote("origin", bare_path.to_str().unwrap())
        .unwrap();
    {
        let mut handle = second.find_remote("origin").unwrap();
        handle
            .fetch(&["+refs/heads/*:refs/remotes/origin/*".to_owned()], None, None)
            .unwrap();
    }
    let tip = second
        .find_reference("refs/remotes/origin/reviewed")
        .expect("the second repository fetched the remote")
        .target()
        .unwrap();
    second
        .reset(
            &second.find_object(tip, None).unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
    {
        let root = second.workdir().unwrap().to_path_buf();
        std::fs::write(root.join("other.txt"), "other\n").unwrap();
        let mut index = second.index().unwrap();
        index.add_path(std::path::Path::new("other.txt")).unwrap();
        index.write().unwrap();
        let tree = second.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = second.head().unwrap().peel_to_commit().unwrap();
        let oid = second
            .commit(None, &sig, &sig, "theirs", &tree, &[&parent])
            .unwrap();
        second
            .reference("refs/heads/reviewed", oid, true, "fixture")
            .unwrap();
    }
    {
        let mut handle = second.find_remote("origin").unwrap();
        handle
            .push(&["refs/heads/reviewed:refs/heads/reviewed"], None)
            .unwrap();
    }
    let moved = read_remote("refs/heads/reviewed").unwrap();
    assert_ne!(
        moved,
        observed,
        "the remote really moved to a different commit"
    );
    drop(second);

    // The push is based on the value observed BEFORE the remote moved.
    let _generation = service
        .push(
            root.clone(),
            "origin".into(),
            format!("refs/heads/{branch}"),
            "refs/heads/reviewed".into(),
            Some(observed),
            Duration::from_secs(10),
        )
        .unwrap();
    match drain(&mut service) {
        GitRemoteOutcome::Refused(GitRemoteRefusal::RemoteMoved {
            destination,
            expected,
            actual,
        }) => {
            assert_eq!(destination, "refs/heads/reviewed");
            assert_eq!(expected, observed, "the lease reports the value it was based on");
            assert_eq!(actual, moved, "and the value the remote actually holds now");
        }
        other => panic!("a push based on a moved remote is refused: {other:?}"),
    }
    assert_eq!(
        read_remote("refs/heads/reviewed"),
        Some(moved),
        "a refused push leaves the remote byte-identical"
    );

    // ---- NON-FORCED: with no lease given, the remote still refuses to lose a commit, so the
    // unpushed local state cannot overwrite what the remote holds.
    let _generation = service
        .push(
            root.clone(),
            "origin".into(),
            format!("refs/heads/{branch}"),
            "refs/heads/reviewed".into(),
            None,
            Duration::from_secs(10),
        )
        .unwrap();
    match drain(&mut service) {
        GitRemoteOutcome::Failed(error) => {
            let message = error.to_string();
            assert!(
                !message.to_ascii_lowercase().contains("auth"),
                "a non-fast-forward refusal is NOT reported as an authentication failure: {message}"
            );
        }
        other => panic!("a non-forced push cannot overwrite the remote: {other:?}"),
    }
    assert_eq!(
        read_remote("refs/heads/reviewed"),
        Some(moved),
        "the remote is unchanged by the refused non-forced push"
    );
    let _ = dir;
}

#[test]
fn conflict_provenance_reports_the_three_sides_from_the_index_on_demand() {
    use crate::workspace::git::read_conflict_state;
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
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
    assert_eq!(clean.phase(), "no operation in progress; 0 conflicted path(s)");

    // Merge theirs into ours and let it conflict for real.
    {
        let their_commit = repo.find_commit(their_tip).unwrap();
        let their_annotated = repo
            .find_annotated_commit(their_commit.id())
            .unwrap();
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
    let repo = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    std::fs::write(root.join("shared.txt"), "base\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
    }
    let base = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
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
    repo.reference("ORIG_HEAD", our_tip, true, "fixture").unwrap();
    (our_tip, their_tip)
}

#[test]
fn continue_refuses_while_conflicts_remain_and_then_records_two_parents() {
    use crate::workspace::git::{GitRecoveryRefusal, continue_operation};
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
    assert_eq!(commit.parent_id(0).unwrap(), our_tip, "first parent is ours");
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
fn abort_returns_to_the_recorded_start_and_refuses_to_discard_work() {
    use crate::workspace::git::{GitRecoveryRefusal, abort_operation, read_conflict_state};
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
    let outcome = abort_operation(&root).expect("a clean abort returns to the recorded start");
    assert_eq!(outcome.operation, "merge");
    assert_eq!(outcome.restored_to, Some(our_tip));
    let repo = git2::Repository::open(&root).unwrap();
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
    use crate::workspace::git::{GitRecoveryRefusal, abort_operation, continue_operation};
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "only.txt", "one\n");
    assert_eq!(
        continue_operation(dir.path(), ""),
        Err(GitRecoveryRefusal::NotPaused("continue")),
        "there is nothing to continue"
    );
    assert_eq!(
        abort_operation(dir.path()),
        Err(GitRecoveryRefusal::NotPaused("abort")),
        "and nothing to abort"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(repo.head().unwrap().target().unwrap())
    );
}

#[test]
fn the_operation_bar_lists_only_the_actions_valid_in_the_repositorys_real_state() {
    use crate::workspace::git::{
        GitConflictChoice, GitOperationAction, GitOperationBar, read_conflict_state,
    };
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
    assert!(bar.phase.contains("shared.txt").eq(&false) || bar.phase.contains("1 conflicted path"));

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

#[test]
fn whole_file_replacement_previews_cancels_and_applies_exactly_its_file_list() {
    use crate::workspace::git::{
        GitConflictChoice, apply_replacement, git_index_fingerprint, plan_replacement,
    };
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

    let plan = plan_replacement(&root, std::slice::from_ref(&conflicted), GitConflictChoice::Theirs)
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
fn a_reviewed_merge_records_two_parents_and_leaves_conflicts_to_the_user() {
    use crate::workspace::git::{
        GitRemoteEvent, GitRemoteOutcome, GitRemoteService, GitUpdateIntegration,
    };
    let dir = tempdir().unwrap();
    let bare = git2::Repository::init_bare(dir.path().join("origin.git")).unwrap();
    let bare_path = std::fs::canonicalize(bare.path()).unwrap();
    drop(bare);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = git2::Repository::init(&work).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    commit(&repo, "f.txt", "one\n");
    repo.remote("origin", bare_path.to_str().unwrap()).unwrap();
    push_master(&repo);
    let root = std::fs::canonicalize(&work).unwrap();
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    let start = repo.head().unwrap().target().unwrap();

    // The remote advances from a second repository on a DIFFERENT path, so the merge is clean.
    let theirs = {
        let path = dir.path().join("second");
        std::fs::create_dir_all(&path).unwrap();
        let second = git2::Repository::init(&path).unwrap();
        second.remote("origin", bare_path.to_str().unwrap()).unwrap();
        {
            let mut handle = second.find_remote("origin").unwrap();
            handle
                .fetch(&["+refs/heads/*:refs/remotes/origin/*".to_owned()], None, None)
                .unwrap();
        }
        let tip = second
            .find_reference(&format!("refs/remotes/origin/{branch}"))
            .unwrap()
            .target()
            .unwrap();
        second
            .reset(&second.find_object(tip, None).unwrap(), git2::ResetType::Hard, None)
            .unwrap();
        let root = second.workdir().unwrap().to_path_buf();
        std::fs::write(root.join("other.txt"), "other\n").unwrap();
        let mut index = second.index().unwrap();
        index.add_path(std::path::Path::new("other.txt")).unwrap();
        index.write().unwrap();
        let tree = second.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = second.head().unwrap().peel_to_commit().unwrap();
        let oid = second
            .commit(None, &sig, &sig, "theirs", &tree, &[&parent])
            .unwrap();
        second
            .reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
        {
            let mut handle = second.find_remote("origin").unwrap();
            handle
                .push(&[format!("refs/heads/{branch}:refs/heads/{branch}")], None)
                .unwrap();
        }
        oid
    };
    // Ours commits locally on a DIFFERENT path, so the two have diverged but do not conflict.
    {
        std::fs::write(root.join("ours.txt"), "ours\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("ours.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "ours", &tree, &[&parent])
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), oid, true, "fixture")
            .unwrap();
    }
    let ours = repo.head().unwrap().target().unwrap();
    assert_ne!(ours, start, "the branch really moved locally");

    let mut service = GitRemoteService::start(4);
    let drain = |service: &mut GitRemoteService| -> GitRemoteOutcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match service.poll() {
                Some(GitRemoteEvent::Fetched { outcome, .. }) => return outcome,
                None => {
                    assert!(Instant::now() < deadline, "remote operation timed out");
                    thread::yield_now();
                }
            }
        }
    };
    let _generation = service
        .fetch(root.clone(), "origin".into(), Duration::from_secs(10))
        .unwrap();
    let _ = drain(&mut service);
    let _generation = service
        .review_update(
            root.clone(),
            "origin".into(),
            branch.clone(),
            Duration::from_secs(10),
        )
        .unwrap();
    let review = match drain(&mut service) {
        GitRemoteOutcome::Reviewed(review) => review,
        other => panic!("a diverged branch reviews as a merge: {other:?}"),
    };
    assert_eq!(review.integration, GitUpdateIntegration::Merge);

    // A CLEAN MERGE records a commit with TWO parents, first ours and second theirs.
    let _generation = service
        .apply_update(review.clone(), Duration::from_secs(10))
        .unwrap();
    let outcome = drain(&mut service);
    let GitRemoteOutcome::Merged { merge, .. } = outcome else {
        panic!("a clean merge is recorded: {outcome:?}");
    };
    let fresh = git2::Repository::open(&root).unwrap();
    let commit = fresh.find_commit(merge).unwrap();
    assert_eq!(
        commit.parent_count(),
        2,
        "the merge commit records BOTH parents"
    );
    assert_eq!(commit.parent_id(0).unwrap(), ours, "first parent is ours");
    assert_eq!(commit.parent_id(1).unwrap(), theirs, "second parent is theirs");
    assert_eq!(
        fresh.state(),
        git2::RepositoryState::Clean,
        "a clean merge leaves no paused operation"
    );

    // THE CONFLICTING CASE: ours and theirs both change the SAME path, so the merge pauses.
    let conflicting = dir.path().join("conflicting");
    std::fs::create_dir_all(&conflicting).unwrap();
    let repo2 = git2::Repository::init(&conflicting).unwrap();
    std::fs::write(conflicting.join("shared.txt"), "base\n").unwrap();
    {
        let mut index = repo2.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo2.find_tree(index.write_tree().unwrap()).unwrap();
        repo2
            .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
    }
    let base = repo2.head().unwrap().target().unwrap();
    let base_commit = repo2.find_commit(base).unwrap();
    repo2
        .branch("remote-side", &base_commit, false)
        .unwrap();
    {
        let mut builder = repo2.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
        let blob = repo2.blob(b"theirs\n").unwrap();
        builder.insert("shared.txt", blob, 0o100644).unwrap();
        let tree = repo2.find_tree(builder.write().unwrap()).unwrap();
        repo2
            .commit(
                Some("refs/heads/remote-side"),
                &sig,
                &sig,
                "theirs",
                &tree,
                &[&base_commit],
            )
            .unwrap();
    }
    std::fs::write(conflicting.join("shared.txt"), "ours\n").unwrap();
    {
        let mut index = repo2.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo2.find_tree(index.write_tree().unwrap()).unwrap();
        repo2
            .commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let our_side = repo2.head().unwrap().target().unwrap();
    let their_side = repo2
        .find_reference("refs/heads/remote-side")
        .unwrap()
        .target()
        .unwrap();
    // Bring the remote side in as a local ref so the merge has something real to join.
    repo2
        .reference(
            "refs/remotes/origin/remote-side",
            their_side,
            true,
            "fixture",
        )
        .unwrap();
    let conflicted_review = crate::workspace::git::GitUpdateReview {
        repository: std::fs::canonicalize(&conflicting).unwrap(),
        remote: "origin".into(),
        branch: "remote-side".into(),
        local: our_side,
        remote_tip: their_side,
        commits: 1,
        integration: GitUpdateIntegration::Merge,
    };
    let _generation = service
        .apply_update(conflicted_review, Duration::from_secs(10))
        .unwrap();
    let outcome = drain(&mut service);
    let GitRemoteOutcome::MergeConflicted { conflicted, .. } = outcome else {
        panic!("a conflicting merge is left paused: {outcome:?}");
    };
    assert_eq!(
        conflicted,
        vec!["shared.txt".to_owned()],
        "the paused merge names the path the user must settle"
    );
    let paused = git2::Repository::open(&conflicting).unwrap();
    assert_eq!(
        paused.state(),
        git2::RepositoryState::Merge,
        "the operation is PAUSED rather than auto-resolved"
    );
    assert_eq!(
        paused.head().unwrap().target(),
        Some(our_side),
        "and HEAD has not moved to a commit the user never chose"
    );
    assert_eq!(
        crate::workspace::git::read_conflict_state(&conflicting)
            .unwrap()
            .conflicts
            .len(),
        1,
        "the conflict is visible to the resolver"
    );
}

#[test]
fn the_bar_names_every_paused_operation_the_contract_lists() {
    use crate::workspace::git::{GitOperationAction, GitOperationBar, read_conflict_state};
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
            bar.offers(GitOperationAction::Continue),
            "with nothing conflicted, continuing is valid for {expected}"
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
fn a_clean_cherry_pick_carries_the_change_and_a_revert_removes_it() {
    use crate::workspace::git::{GitPickKind, GitPickOutcome, apply_pick};
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    // A side branch adds a file; the main line does not have it.
    commit(&repo, "base.txt", "base\n");
    let base = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    repo.branch("side", &base_commit, false).unwrap();
    let picked = {
        let root = repo.workdir().unwrap().to_path_buf();
        std::fs::write(root.join("picked.txt"), "picked\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("picked.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "side commit", &tree, &[&base_commit])
            .unwrap();
        repo.reference("refs/heads/side", oid, true, "fixture")
            .unwrap();
        repo.reference("refs/heads/master", base, true, "fixture")
            .unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        repo.checkout_head(Some(&mut checkout)).unwrap();
        oid
    };
    let root = std::fs::canonicalize(&root).unwrap();
    assert!(
        !root.join("picked.txt").exists(),
        "the main line really does not have the change yet"
    );

    // CLEAN CHERRY-PICK: HEAD moves and the tree actually carries the change.
    let outcome = apply_pick(&root, GitPickKind::CherryPick, &picked.to_string()).unwrap();
    let GitPickOutcome::Applied { commit, .. } = outcome else {
        panic!("a non-conflicting pick applies cleanly: {outcome:?}");
    };
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(
        fresh.head().unwrap().target(),
        Some(commit),
        "HEAD moved to the recorded pick"
    );
    assert!(
        root.join("picked.txt").is_file(),
        "and the working tree really carries the picked change"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("picked.txt")).unwrap(),
        "picked\n"
    );
    assert_eq!(
        fresh.state(),
        git2::RepositoryState::Clean,
        "a clean pick leaves no paused operation"
    );
    let recorded = fresh.find_commit(commit).unwrap();
    assert_eq!(recorded.parent_count(), 1, "a pick has one parent");
    assert_eq!(
        recorded.parent_id(0).unwrap(),
        base,
        "which is the branch it was picked onto, not the picked commit"
    );

    // CLEAN REVERT of that same commit removes the change again.
    let outcome = apply_pick(&root, GitPickKind::Revert, &commit.to_string()).unwrap();
    let GitPickOutcome::Applied { commit: reverted, .. } = outcome else {
        panic!("a non-conflicting revert applies cleanly: {outcome:?}");
    };
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(fresh.head().unwrap().target(), Some(reverted));
    assert!(
        !root.join("picked.txt").exists(),
        "the revert removed the change from the working tree"
    );
    assert!(
        fresh.find_commit(reverted).unwrap().parent_count() == 1,
        "and recorded a single-parent commit"
    );
}

#[test]
fn a_conflicting_pick_pauses_and_reverting_a_merge_is_refused_with_its_reason() {
    use crate::workspace::git::{GitPickKind, GitPickOutcome, GitPickRefusal, apply_pick};
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    std::fs::write(root.join("shared.txt"), "base\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
    }
    let base = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    // A side commit changes shared.txt one way...
    repo.branch("side", &base_commit, false).unwrap();
    let side = {
        std::fs::write(root.join("shared.txt"), "side\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        // Committed WITHOUT `Some("HEAD")`: that would move the checked-out branch too, and the
        // main-line commit below would then have the wrong first parent.
        let oid = repo
            .commit(None, &sig, &sig, "side", &tree, &[&base_commit])
            .unwrap();
        repo.reference("refs/heads/side", oid, true, "fixture")
            .unwrap();
        oid
    };
    // ...and the main line changes it differently, so picking conflicts.
    std::fs::write(root.join("shared.txt"), "main\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "main", &tree, &[&base_commit])
            .unwrap();
    }
    let main_tip = repo.head().unwrap().target().unwrap();
    let root = std::fs::canonicalize(&root).unwrap();

    // A CONFLICTING PICK pauses, and names the path the user must settle.
    let outcome = apply_pick(&root, GitPickKind::CherryPick, &side.to_string()).unwrap();
    let GitPickOutcome::Conflicted { operation, conflicted } = outcome else {
        panic!("a conflicting pick pauses: {outcome:?}");
    };
    assert_eq!(operation, GitPickKind::CherryPick);
    assert_eq!(conflicted, vec!["shared.txt".to_owned()]);
    let paused = git2::Repository::open(&root).unwrap();
    assert_eq!(
        paused.state(),
        git2::RepositoryState::CherryPick,
        "the repository is paused in a cherry-pick, which the bar already names"
    );
    assert_eq!(
        paused.head().unwrap().target(),
        Some(main_tip),
        "and HEAD has not moved to a commit the user never chose"
    );
    assert_eq!(
        crate::workspace::git::read_conflict_state(&root)
            .unwrap()
            .operation
            .as_deref(),
        Some("cherry-pick"),
        "the conflict model names it"
    );

    // REVERTING A MERGE IS REFUSED with its reason, leaving HEAD and the worktree as they were.
    let merge = {
        let paused_repo = git2::Repository::open(&root).unwrap();
        paused_repo.cleanup_state().unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        paused_repo.checkout_head(Some(&mut checkout)).unwrap();
        let side_commit = paused_repo.find_commit(side).unwrap();
        let annotated = paused_repo.find_annotated_commit(side_commit.id()).unwrap();
        paused_repo.merge(&[&annotated], None, None).unwrap();
        drop(annotated);
        let mut index = paused_repo.index().unwrap();
        std::fs::write(root.join("shared.txt"), "merged\n").unwrap();
        index.add_path(std::path::Path::new("shared.txt")).unwrap();
        index.write().unwrap();
        let tree = paused_repo.find_tree(index.write_tree().unwrap()).unwrap();
        let head = paused_repo.head().unwrap().peel_to_commit().unwrap();
        let oid = paused_repo
            .commit(None, &sig, &sig, "merge", &tree, &[&head, &side_commit])
            .unwrap();
        paused_repo
            .reference("refs/heads/master", oid, true, "fixture")
            .unwrap();
        paused_repo.cleanup_state().unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        paused_repo.checkout_head(Some(&mut checkout)).unwrap();
        oid
    };
    let before_head = git2::Repository::open(&root).unwrap().head().unwrap().target();
    let before_file = std::fs::read(root.join("shared.txt")).unwrap();
    let refusal = apply_pick(&root, GitPickKind::Revert, &merge.to_string())
        .expect_err("reverting a merge is refused");
    assert!(
        matches!(refusal, GitPickRefusal::MergeCommitNeedsParent { .. }),
        "the refusal is the merge-parent one: {refusal:?}"
    );
    let message = refusal.to_string();
    assert!(
        message.contains("needs a chosen parent") && message.contains("will not pick one for you"),
        "and it says why, naming the limit: {message}"
    );
    assert_eq!(
        git2::Repository::open(&root).unwrap().head().unwrap().target(),
        before_head,
        "a refused revert leaves HEAD byte-identical"
    );
    assert_eq!(
        std::fs::read(root.join("shared.txt")).unwrap(),
        before_file,
        "and the working tree untouched"
    );
}

#[test]
fn each_reset_mode_changes_exactly_what_it_claims_and_hard_refuses_to_discard_work() {
    use crate::workspace::git::{GitResetMode, GitResetRefusal, git_index_fingerprint, reset_to};
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    commit(&repo, "f.txt", "one\n");
    let first = repo.head().unwrap().target().unwrap();
    // The shared helper writes an UNBORN-HEAD commit, so the second one names its parent.
    let commit_next = |repo: &git2::Repository, path: &str, text: &str| {
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join(path), text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[&parent])
            .unwrap();
        repo.head().unwrap().target().unwrap()
    };
    let second = commit_next(&repo, "f.txt", "two\n");
    let root = std::fs::canonicalize(&root).unwrap();

    // SOFT: HEAD moves; the index and the file keep their EXACT bytes.
    let index_before = git_index_fingerprint(&root).unwrap();
    let bytes_before = std::fs::read(root.join("f.txt")).unwrap();
    let outcome = reset_to(&root, GitResetMode::Soft, &first.to_string()).unwrap();
    assert_eq!(outcome.from, second);
    assert_eq!(outcome.to, first);
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(fresh.head().unwrap().target(), Some(first), "HEAD moved");
    assert_eq!(
        git_index_fingerprint(&root).unwrap(),
        index_before,
        "a soft reset leaves the index byte-identical"
    );
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        bytes_before,
        "and the working tree byte-identical"
    );
    assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "two\n");

    // MIXED: HEAD and the index move; the WORKTREE bytes stay identical.
    reset_to(&root, GitResetMode::Soft, &second.to_string()).unwrap();
    let bytes_before = std::fs::read(root.join("f.txt")).unwrap();
    reset_to(&root, GitResetMode::Mixed, &first.to_string()).unwrap();
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(fresh.head().unwrap().target(), Some(first));
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        bytes_before,
        "a mixed reset leaves the working tree byte-identical: the change becomes unstaged"
    );
    let statuses = fresh.statuses(None).unwrap();
    assert!(
        !statuses.is_empty(),
        "and the difference is now visible as an unstaged change"
    );

    // HARD WITH UNCOMMITTED WORK: REFUSED, naming the path, leaving everything untouched.
    let head_before = fresh.head().unwrap().target();
    let index_before = git_index_fingerprint(&root).unwrap();
    let bytes_before = std::fs::read(root.join("f.txt")).unwrap();
    let refusal = reset_to(&root, GitResetMode::Hard, &second.to_string())
        .expect_err("a hard reset must not discard uncommitted work");
    match &refusal {
        GitResetRefusal::WouldDiscard(paths) => assert!(
            paths.contains(&"f.txt".to_owned()),
            "the refusal names the path it would discard: {paths}"
        ),
        other => panic!("a hard reset with work is refused: {other:?}"),
    }
    assert!(
        refusal.to_string().contains("never throws work away"),
        "and says why: {refusal}"
    );
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(
        fresh.head().unwrap().target(),
        head_before,
        "a refused hard reset leaves HEAD untouched"
    );
    assert_eq!(
        git_index_fingerprint(&root).unwrap(),
        index_before,
        "and the index untouched"
    );
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        bytes_before,
        "and the working tree untouched"
    );

    // HARD ON A CLEAN TREE: applies, moving HEAD and the worktree together.
    {
        let fresh = git2::Repository::open(&root).unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        fresh.checkout_head(Some(&mut checkout)).unwrap();
    }
    let outcome = reset_to(&root, GitResetMode::Hard, &second.to_string()).unwrap();
    assert_eq!(outcome.mode, GitResetMode::Hard);
    let fresh = git2::Repository::open(&root).unwrap();
    assert_eq!(fresh.head().unwrap().target(), Some(second));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "two\n",
        "a clean hard reset moves the worktree with HEAD"
    );
    // The recovery limit is stated per mode rather than for reset in general.
    assert!(GitResetMode::Soft.recovery_limit().contains("nothing is lost"));
    assert!(GitResetMode::Mixed.recovery_limit().contains("unstaged"));
    assert!(GitResetMode::Hard.recovery_limit().contains("DISCARDED"));
    assert!(GitResetMode::Hard.recovery_limit().contains("reflog"));
}

#[test]
fn a_handoff_informs_without_touching_the_repository_and_names_the_two_reasons() {
    use crate::workspace::git::{GitHandoff, git_index_fingerprint};
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    commit(&repo, "f.txt", "one\n");
    let root = std::fs::canonicalize(&root).unwrap();
    std::fs::write(root.join("f.txt"), "modified\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "new\n").unwrap();

    // A handoff is INFORMATION: it is not an action, so nothing may change.
    let head_before = repo.head().unwrap().target();
    let index_before = git_index_fingerprint(&root).unwrap();
    let file_before = std::fs::read(root.join("f.txt")).unwrap();
    let untracked_before = std::fs::read(root.join("untracked.txt")).unwrap();

    for handoff in GitHandoff::ALL {
        let disposition = handoff.disposition();
        // It names the operation, the command to run, and the limits of the editor's involvement.
        assert!(
            disposition.contains(handoff.name()),
            "the disposition names the operation: {disposition}"
        );
        assert!(
            disposition.contains(handoff.command()),
            "and gives the EXACT command to run: {disposition}"
        );
        assert!(
            disposition.contains("does not execute it")
                && disposition.contains("does not track its state")
                && disposition.contains("does not report its outcome as native completion"),
            "and states all three limits rather than implying support: {disposition}"
        );
        // The refusal text says NOT performed, so a handoff is never presented as native.
        assert!(
            disposition.contains("NOT performed by this editor"),
            "a handoff is never presented as native completion: {disposition}"
        );
    }

    // The TWO reasons are different facts and are kept apart.
    assert!(
        GitHandoff::Bisect.reason().contains("no bisect API"),
        "bisect is refused because libgit2 has no API: {}",
        GitHandoff::Bisect.reason()
    );
    assert!(
        GitHandoff::Lfs.reason().contains("no LFS filter API"),
        "LFS likewise: {}",
        GitHandoff::Lfs.reason()
    );
    assert!(
        GitHandoff::Submodules.reason().contains("partial")
            && GitHandoff::Submodules.reason().contains("no complete"),
        "while submodules have a PARTIAL API rather than none: {}",
        GitHandoff::Submodules.reason()
    );
    assert!(
        !GitHandoff::Submodules.reason().contains("no submodule API"),
        "and that difference is not flattened into the same sentence"
    );

    assert_eq!(repo.head().unwrap().target(), head_before, "HEAD is untouched");
    assert_eq!(
        git_index_fingerprint(&root).unwrap(),
        index_before,
        "the index is untouched"
    );
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        file_before,
        "the tracked file is byte-identical"
    );
    assert_eq!(
        std::fs::read(root.join("untracked.txt")).unwrap(),
        untracked_before,
        "and an untracked file is not touched either"
    );
}

#[test]
fn a_pick_preview_names_what_would_change_and_refuses_a_revision_that_moved() {
    use crate::workspace::git::{
        GitPickKind, GitPickOutcome, GitPickRefusal, apply_reviewed_pick, plan_pick,
    };
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    commit(&repo, "base.txt", "base\n");
    let base = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    repo.branch("side", &base_commit, false).unwrap();
    let picked = {
        // Built from TREES rather than by staging, so the fixture leaves the worktree and the
        // index clean and the test measures the pick rather than the fixture.
        let mut builder = repo.treebuilder(Some(&base_commit.tree().unwrap())).unwrap();
        let blob = repo.blob(b"picked\n").unwrap();
        builder.insert("picked.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "add picked file", &tree, &[&base_commit])
            .unwrap();
        repo.reference("refs/heads/side", oid, true, "fixture")
            .unwrap();
        oid
    };
    let root = std::fs::canonicalize(&root).unwrap();

    // THE PREVIEW names the commit, its subject, the paths it would change and the HEAD it was
    // computed against - all BEFORE anything moves.
    let preview = plan_pick(&root, GitPickKind::CherryPick, &picked.to_string()).unwrap();
    assert_eq!(preview.target, picked);
    assert_eq!(preview.subject, "add picked file");
    assert_eq!(
        preview.changes,
        vec!["picked.txt".to_owned()],
        "the preview names the path the pick would add"
    );
    assert_eq!(preview.head, base, "and the HEAD it was computed against");
    let summary = preview.summary();
    assert!(
        summary.contains("cherry-pick") && summary.contains("picked.txt"),
        "the summary names the operation and the change: {summary}"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(base),
        "previewing moves nothing"
    );
    assert!(!root.join("picked.txt").exists(), "and writes no file");

    // STALE: HEAD moves after the preview, so the apply is REFUSED against the named value.
    let moved = {
        let parent = repo.find_commit(base).unwrap();
        let mut builder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
        let blob = repo.blob(b"moved\n").unwrap();
        builder.insert("moved.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "moved", &tree, &[&parent])
            .unwrap();
        repo.reference("refs/heads/master", oid, true, "fixture")
            .or_else(|_| repo.reference("refs/heads/main", oid, true, "fixture"))
            .unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        repo.checkout_head(Some(&mut checkout)).unwrap();
        oid
    };
    assert_ne!(moved, base);
    let refusal = apply_reviewed_pick(&root, &preview)
        .expect_err("a preview against a moved HEAD is refused");
    match &refusal {
        GitPickRefusal::StalePreview {
            expected, actual, ..
        } => {
            assert_eq!(
                expected,
                &base.to_string(),
                "the refusal reports the revision the preview NAMED"
            );
            assert_eq!(actual, &moved.to_string(), "and where HEAD actually is");
        }
        other => panic!("a moved HEAD refuses the previewed pick: {other:?}"),
    }
    assert!(
        !root.join("picked.txt").exists(),
        "a refused pick writes nothing"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        Some(moved),
        "and leaves the branch where it had moved to"
    );

    // A FRESH preview against the moved HEAD applies.
    let fresh = plan_pick(&root, GitPickKind::CherryPick, &picked.to_string()).unwrap();
    assert_eq!(fresh.head, moved, "the new preview names the new HEAD");
    let outcome = apply_reviewed_pick(&root, &fresh).expect("a fresh preview applies");
    let GitPickOutcome::Applied { commit, .. } = outcome else {
        panic!("a clean pick applies: {outcome:?}");
    };
    assert!(
        root.join("picked.txt").is_file(),
        "and the change arrives in the working tree"
    );
    assert_eq!(repo.head().unwrap().target(), Some(commit));
    // A MERGE REVERT is refused at PREVIEW time, so the preview never offers what apply refuses.
    let merge = {
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let side_commit = repo.find_commit(picked).unwrap();
        // A two-parent commit by hand, so no merge state or staged change is left behind.
        let mut builder = repo.treebuilder(Some(&head.tree().unwrap())).unwrap();
        let blob = repo.blob(b"merged\n").unwrap();
        builder.insert("merged.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "merge", &tree, &[&head, &side_commit])
            .unwrap();
        repo.reference("refs/heads/master", oid, true, "fixture")
            .or_else(|_| repo.reference("refs/heads/main", oid, true, "fixture"))
            .unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        repo.checkout_head(Some(&mut checkout)).unwrap();
        oid
    };
    assert_eq!(
        plan_pick(&root, GitPickKind::Revert, &merge.to_string()),
        Err(GitPickRefusal::MergeCommitNeedsParent {
            operation: "revert",
            commit: merge.to_string(),
        }),
        "the preview refuses a merge revert for the same reason the apply does"
    );
}

#[test]
fn a_review_worktree_is_detached_at_the_inspected_revision_and_moves_no_branch() {
    use crate::workspace::git::GitBranchRequest as R;
    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "w.txt", "one\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
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

    let branches_before: Vec<String> = repo
        .branches(None)
        .unwrap()
        .map(|branch| branch.unwrap().0.name().unwrap().unwrap().to_owned())
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
    let branches_after: Vec<String> = repo
        .branches(None)
        .unwrap()
        .map(|branch| branch.unwrap().0.name().unwrap().unwrap().to_owned())
        .collect();
    assert_eq!(
        branches_before, branches_after,
        "looking at a review creates no branch"
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


/// The parser that draws diffs must read the text THIS module's producer emits, not merely
/// text that looks like a patch. This drives the real service and parses its real output, so
/// the renderer's input is proved against the producer rather than against a fixture that
/// only resembles it.
#[test]
fn the_unified_diff_parser_reads_the_patch_text_this_producer_emits() {
    use crate::ui::{DiffLineKind, parse_unified_patch};

    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "f.txt", "one\ntwo\nthree\n");
    commit_next(&repo, "f.txt", "one\ntwo changed\nthree\n");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 1));
    let tip = page.entries[0].id;
    let patch = log_patch(crate::workspace::git::GitLogPatchRequest::Commit {
        repository: root.clone(),
        commit: tip,
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    });
    assert!(!patch.binary, "a text change is not binary");
    let parsed = parse_unified_patch(&patch.text, "test");
    assert_eq!(parsed.hunks.len(), 1, "one changed line is one hunk");
    let lines = &parsed.hunks[0].lines;
    let removed = lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Removed)
        .expect("the producer emits a removal");
    let added = lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Added)
        .expect("the producer emits an addition");
    assert_eq!(removed.text, "two");
    assert_eq!(added.text, "two changed");
    assert_eq!(
        (removed.old_number, removed.new_number),
        (Some(2), None),
        "a removal has an old number and no new number"
    );
    assert_eq!(
        (added.old_number, added.new_number),
        (None, Some(2)),
        "an addition has a new number and no old number"
    );
    let context = lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Context)
        .expect("the producer emits context lines");
    assert!(
        context.old_number.is_some() && context.new_number.is_some(),
        "a context line carries both numbers"
    );
    // The intra-line span covers exactly the changed word, computed from the producer's own
    // output rather than from a hand-written pair.
    assert_eq!(added.spans.len(), 1, "the changed word is marked");
    let span = added.spans[0];
    assert_eq!(&added.text[span.start..span.end], " changed");
    assert!(
        removed.spans.is_empty(),
        "a removal whose text is a prefix of the addition marks nothing"
    );
}

/// The producer's no-trailing-newline path was established by probing libgit2's callback
/// rather than by a test, so this closes it: the marker survives into the patch text and the
/// lines around it parse with the right numbers.
#[test]
fn a_file_with_no_trailing_newline_keeps_its_marker_and_parses() {
    use crate::ui::{DiffLineKind, parse_unified_patch};

    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    // Neither revision ends with a newline, so the producer emits the marker.
    commit(&repo, "f.txt", "one\ntwo");
    commit_next(&repo, "f.txt", "one\nthree");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 1));
    let tip = page.entries[0].id;
    let patch = log_patch(crate::workspace::git::GitLogPatchRequest::Commit {
        repository: root.clone(),
        commit: tip,
        maximum_bytes: 64 * 1024,
        deadline: Duration::from_secs(5),
    });

    assert!(
        patch.text.contains("\\ No newline at end of file"),
        "the marker survives into the patch text: {:?}",
        patch.text
    );
    let parsed = parse_unified_patch(&patch.text, "test");
    assert_eq!(parsed.hunks.len(), 1, "one changed line is one hunk");
    let lines = &parsed.hunks[0].lines;
    assert!(
        !lines.iter().any(|line| line.text.contains("No newline")),
        "the marker is not a diff line of its own: {:?}",
        lines.iter().map(|line| &line.text).collect::<Vec<_>>()
    );
    let removed = lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Removed)
        .expect("a removal");
    let added = lines
        .iter()
        .find(|line| line.kind == DiffLineKind::Added)
        .expect("an addition");
    assert_eq!(
        (removed.text.as_str(), removed.old_number, removed.new_number),
        ("two", Some(2), None)
    );
    assert_eq!(
        (added.text.as_str(), added.old_number, added.new_number),
        ("three", None, Some(2))
    );
}

/// U-17: a real merge produces a BEND — an edge that leaves the commit's lane and arrives at
/// another — which is what the legacy string prefix could never express.
#[test]
fn a_real_merge_produces_a_bend_edge_and_a_distinct_merge_glyph() {
    use crate::workspace::git::{GraphCellKind, history_graph_rows};

    let dir = tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    commit(&repo, "g-base.txt", "base\n");
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let branch = |name: &str, reference: &str| {
        std::fs::write(dir.path().join(name), "side\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some(reference), &sig, &sig, name, &tree, &[&base])
            .unwrap()
    };
    let left = branch("g-left.txt", "refs/heads/gleft");
    let right = branch("g-right.txt", "refs/heads/gright");
    let left = repo.find_commit(left).unwrap();
    let right = repo.find_commit(right).unwrap();
    std::fs::write(dir.path().join("g-left.txt"), "side\n").unwrap();
    std::fs::write(dir.path().join("g-right.txt"), "side\n").unwrap();
    let mut merged = repo.index().unwrap();
    merged.add_path(Path::new("g-left.txt")).unwrap();
    merged.add_path(Path::new("g-right.txt")).unwrap();
    merged.write().unwrap();
    let tree = repo.find_tree(merged.write_tree().unwrap()).unwrap();
    let head_name = repo.head().unwrap().name().unwrap().to_owned();
    let merge = repo
        .commit(None, &sig, &sig, "g-merge", &tree, &[&left, &right])
        .unwrap();
    repo.reference(&head_name, merge, true, "g-merge").unwrap();

    let root = std::fs::canonicalize(dir.path()).unwrap();
    let page = log_page(log_page_request(&root, None, 0, 10));
    assert!(page.entries[0].merge, "the fixture's tip is a merge");
    let rows = history_graph_rows(&page.entries);
    let merge_row = &rows[0];
    assert!(merge_row.merge, "the row knows its commit is a merge");
    assert_eq!(
        merge_row.cells[merge_row.lane],
        GraphCellKind::Merge,
        "the merge sits on its own lane as a merge cell"
    );
    // The bend is the edge that leaves this commit's lane and arrives at another.
    let bend = merge_row
        .edges
        .iter()
        .find(|edge| edge.from == merge_row.lane && edge.to != merge_row.lane)
        .expect("a merge bends an edge into a second lane");
    assert!(
        bend.to > bend.from,
        "the second parent opens a lane to the right: {bend:?}"
    );
    assert!(
        merge_row.cells.contains(&GraphCellKind::BendRight),
        "the row carries a bend cell: {:?}",
        merge_row.cells
    );
    // An ordinary commit on the same history is not a merge cell.
    let plain = rows
        .iter()
        .find(|row| !row.merge)
        .expect("an ordinary commit");
    assert!(
        plain.cells.contains(&GraphCellKind::Commit),
        "an ordinary commit is drawn as a commit rather than a merge"
    );
}
