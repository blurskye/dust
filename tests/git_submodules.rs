use dusty::git::{
    GitBranchList, GitBranchListRequest, GitBranchRequest, GitBranchResult, GitBranchWorker,
};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Submodule Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    git(root, &["init", "-b", "main"]);
    std::fs::write(root.join("tracked.txt"), "initial\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-m", "Initial commit"]);
}

fn add_submodule(root: &Path, source: &Path) {
    // File transport is allowed only on this explicit, disposable local add command.
    git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "library",
            source.to_str().unwrap(),
            "vendor/library with spaces",
        ],
    );
    git(root, &["commit", "-m", "Add local submodule"]);
}

fn list(root: &Path) -> GitBranchList {
    let mut worker = GitBranchWorker::default();
    let request_id = worker
        .start(GitBranchRequest::List(GitBranchListRequest {
            repository: root.to_path_buf(),
            deadline: Duration::from_secs(5),
        }))
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(120);
    loop {
        match worker.poll().unwrap() {
            Some(GitBranchResult::List(list)) => {
                assert_eq!(list.request_id, request_id);
                return list.clone();
            }
            Some(other) => panic!("unexpected branch result: {other:?}"),
            None => {}
        }
        assert!(Instant::now() < expires, "submodule listing did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn distinguishes_clean_dirty_files_and_changed_submodule_head() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    let source = owner.path().join("source");
    repository(&root);
    repository(&source);
    add_submodule(&root, &source);
    let child = root.join("vendor/library with spaces");
    let expected = git(&child, &["rev-parse", "HEAD"]);

    let clean = list(&root);
    assert!(
        clean
            .branches
            .iter()
            .any(|branch| branch.name == "main" && branch.head)
    );
    assert_eq!(clean.worktrees.len(), 1);
    assert!(!clean.worktrees[0].dirty);
    let entries = clean.submodules.unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.name, "library");
    assert_eq!(entry.path, Path::new("vendor/library with spaces"));
    assert!(entry.initialized);
    assert!(!entry.dirty);
    assert_eq!(entry.head.as_deref(), Some(expected.as_str()));
    assert_eq!(entry.index.as_deref(), Some(expected.as_str()));

    std::fs::write(child.join("tracked.txt"), "changed\n").unwrap();
    let dirty = list(&root);
    assert!(dirty.worktrees[0].dirty);
    let entries = dirty.submodules.unwrap();
    assert!(entries[0].dirty);
    assert_eq!(entries[0].head, entries[0].index);

    git(&child, &["add", "tracked.txt"]);
    assert!(list(&root).submodules.unwrap()[0].dirty);
    git(&child, &["commit", "-m", "Advance only the submodule"]);
    let actual = git(&child, &["rev-parse", "HEAD"]);
    assert_ne!(actual, expected);
    let changed_head = list(&root);
    assert!(changed_head.worktrees[0].dirty);
    let entries = changed_head.submodules.unwrap();
    assert!(entries[0].initialized);
    assert!(!entries[0].dirty, "a different HEAD is not a dirty file");
    assert_eq!(entries[0].head.as_deref(), Some(actual.as_str()));
    assert_eq!(entries[0].index.as_deref(), Some(expected.as_str()));

    std::fs::write(child.join("untracked.txt"), "untracked\n").unwrap();
    assert!(list(&root).submodules.unwrap()[0].dirty);
    std::fs::remove_file(child.join("untracked.txt")).unwrap();
    git(&root, &["add", "vendor/library with spaces"]);
    let entries = list(&root).submodules.unwrap();
    assert!(!entries[0].dirty);
    assert_eq!(entries[0].head.as_deref(), Some(actual.as_str()));
    assert_eq!(entries[0].index.as_deref(), Some(actual.as_str()));
}

#[test]
fn keeps_deinitialized_and_configuration_only_submodules_visible() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    let source = owner.path().join("source");
    repository(&root);
    repository(&source);
    add_submodule(&root, &source);
    let expected = git(&source, &["rev-parse", "HEAD"]);
    git(
        &root,
        &[
            "submodule",
            "deinit",
            "--force",
            "--",
            "vendor/library with spaces",
        ],
    );
    git(
        &root,
        &[
            "config",
            "--file",
            ".gitmodules",
            "submodule.pending.path",
            "vendor/pending",
        ],
    );
    git(
        &root,
        &[
            "config",
            "--file",
            ".gitmodules",
            "submodule.pending.url",
            "../source",
        ],
    );

    let entries = list(&root).submodules.unwrap();
    assert_eq!(entries.len(), 2);
    let deinitialized = entries
        .iter()
        .find(|entry| entry.name == "library")
        .unwrap();
    assert_eq!(deinitialized.path, Path::new("vendor/library with spaces"));
    assert!(!deinitialized.initialized);
    assert!(!deinitialized.dirty);
    assert_eq!(deinitialized.head, None);
    assert_eq!(deinitialized.index.as_deref(), Some(expected.as_str()));
    let pending = entries
        .iter()
        .find(|entry| entry.name == "pending")
        .unwrap();
    assert_eq!(pending.path, Path::new("vendor/pending"));
    assert!(!pending.initialized);
    assert!(!pending.dirty);
    assert_eq!(pending.head, None);
    assert_eq!(pending.index, None);
}

#[test]
fn ignores_unsafe_configured_names_and_paths_before_opening_metadata() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    let outside = owner.path().join("outside");
    repository(&root);
    repository(&outside);
    let modules = root.join(".gitmodules");
    std::fs::write(&modules, "").unwrap();
    let mut config = git2::Config::open(&modules).unwrap();
    for (name, path) in [
        ("group/safe", "vendor/safe"),
        ("../outside", "vendor/unsafe-name"),
        (outside.to_str().unwrap(), "vendor/absolute-name"),
        ("parent-path", "../outside"),
        ("embedded-parent", "vendor/../../outside"),
        ("absolute-path", outside.to_str().unwrap()),
        ("windows-parent", "..\\outside"),
        ("windows-absolute", "C:\\outside"),
        ("git-metadata", ".git"),
    ] {
        config
            .set_str(&format!("submodule.{name}.path"), path)
            .unwrap();
        config
            .set_str(&format!("submodule.{name}.url"), "../outside")
            .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&outside, root.join("escape")).unwrap();
        symlink(owner.path().join("missing"), root.join("dangling")).unwrap();
        std::fs::create_dir_all(root.join(".git/modules")).unwrap();
        symlink(&outside, root.join(".git/modules/metadata-escape")).unwrap();
        for (name, path) in [
            ("symlink-path", "escape"),
            ("missing-below-symlink", "escape/missing"),
            ("dangling-path", "dangling"),
            ("metadata-escape", "vendor/metadata-escape"),
        ] {
            config
                .set_str(&format!("submodule.{name}.path"), path)
                .unwrap();
            config
                .set_str(&format!("submodule.{name}.url"), "../outside")
                .unwrap();
        }
    }
    drop(config);

    let page = list(&root);
    assert!(page.branches.iter().any(|branch| branch.name == "main"));
    assert_eq!(page.worktrees.len(), 1);
    let entries = page.submodules.unwrap();
    assert_eq!(
        entries.len(),
        1,
        "unsafe entries must not be opened: {entries:?}"
    );
    assert_eq!(entries[0].name, "group/safe");
    assert_eq!(entries[0].path, Path::new("vendor/safe"));
    assert!(!entries[0].initialized);
    assert_eq!(entries[0].head, None);
}

#[test]
fn malformed_submodule_configuration_does_not_discard_branches_or_worktrees() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    let linked = owner.path().join("linked");
    repository(&root);
    git(
        &root,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    for malformed in [
        "[submodule \"broken\"\npath = vendor/broken\n",
        "[submodule \"broken\"]\npath = vendor/broken\nurl = ../source\nignore = invalid\n",
        "[submodule \"broken\"]\npath\nurl = ../source\n",
        "[submodule \"broken\"]\nurl = ../source\n",
    ] {
        std::fs::write(root.join(".gitmodules"), malformed).unwrap();
        let page = list(&root);
        assert!(page.branches.iter().any(|branch| branch.name == "main"));
        assert!(page.branches.iter().any(|branch| branch.name == "feature"));
        assert_eq!(page.worktrees.len(), 2);
        assert!(
            page.worktrees
                .iter()
                .any(|worktree| worktree.main && worktree.dirty)
        );
        assert!(
            page.worktrees.iter().any(|worktree| {
                !worktree.main && worktree.branch.as_deref() == Some("feature")
            })
        );
        let error = page
            .submodules
            .expect_err("malformed configuration must remain an error");
        assert!(!error.is_empty());
        // The submodule inventory belongs to the requested checkout, not always main.
        let linked_page = list(&linked);
        assert!(linked_page.submodules.unwrap().is_empty());
        assert_eq!(linked_page.worktrees.len(), 2);
    }
}
