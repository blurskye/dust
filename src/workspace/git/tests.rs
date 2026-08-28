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
                status: GitFileStatus::Untracked
            },
            GitStatusEntry {
                relative_path: "tracked.txt".into(),
                status: GitFileStatus::Modified
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
        },
        GitStatusEntry {
            relative_path: "src/nested/b.rs".into(),
            status: GitFileStatus::Conflicted,
        },
        GitStatusEntry {
            relative_path: "docs/readme.md".into(),
            status: GitFileStatus::Untracked,
        },
        GitStatusEntry {
            relative_path: "../escape".into(),
            status: GitFileStatus::Conflicted,
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
