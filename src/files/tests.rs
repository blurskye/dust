use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use crate::editor::text::TextBuffer;
use tempfile::tempdir;

use super::directory::{list_scoped_directory, natural_name_key};
use super::recovery::{
    RECOVERY_DIRECTORY_ENTRY_CEILING, RECOVERY_JOURNAL_BYTE_CEILING, RECOVERY_PAYLOAD_BYTE_CEILING,
    bounded_recovery_entries, read_bounded_bytes, read_bounded_utf8, recovery_journal_size_allowed,
    recovery_payload_size_allowed,
};
use super::vfs::checked_atomic_write;
use super::watch::watch_name_bytes;
use super::{
    DirectoryEntryKind, DirectoryService, ExplorerMutation, ExplorerWatchEntryKind,
    ExplorerWatchError, ExplorerWatchPolicy, ExplorerWatchService, ProjectRootService,
    ProjectRootStrategy, RecoveryRecord, VfsError, VfsResult, VfsWorker, WorkspaceSearchEvent,
    WorkspaceSearchMatch, WorkspaceSearchPolicy, WorkspaceSearchService, WorkspaceSearchSummary,
    WriteExpectation, atomic_write, cleanup_recovery, diff_explorer_fingerprints,
    discover_recovery, explorer_directory_fingerprint, write_recovery,
};

#[test]
fn project_root_resolution_requires_valid_tool_evidence_and_prefers_the_nested_project() {
    let workspace = tempdir().unwrap();
    let nested = workspace.path().join("nested");
    let source = nested.join("src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"nested\"]\n",
    )
    .unwrap();
    fs::write(
        nested.join("Cargo.toml"),
        "[package]\nname = \"nested\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let document = source.join("main.rs");
    fs::write(&document, "fn main() {}\n").unwrap();
    let workspace = fs::canonicalize(workspace.path()).unwrap();
    let document = fs::canonicalize(document).unwrap();
    let mut service = ProjectRootService::default();
    let generation = service
        .resolve(
            document.clone(),
            workspace.clone(),
            7,
            vec!["Cargo.toml".into()],
            ProjectRootStrategy::Cargo,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let resolution = loop {
        if let Some(result) = service.try_result() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "project-root worker timed out");
        std::thread::yield_now();
    };
    assert_eq!(resolution.generation, generation);
    assert_eq!(resolution.root_revision, 7);
    assert_eq!(resolution.document, document);
    assert_eq!(resolution.project_root, Some(workspace.join("nested")));

    let broken_source = workspace.join("broken/src");
    fs::create_dir_all(&broken_source).unwrap();
    fs::write(workspace.join("broken/Cargo.toml"), "[package\n").unwrap();
    let broken = broken_source.join("lib.rs");
    fs::write(&broken, "pub fn broken() {}\n").unwrap();
    service
        .resolve(
            fs::canonicalize(broken).unwrap(),
            workspace.clone(),
            8,
            vec!["Cargo.toml".into()],
            ProjectRootStrategy::Cargo,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let resolution = loop {
        if let Some(result) = service.try_result() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "project-root worker timed out");
        std::thread::yield_now();
    };
    assert_eq!(resolution.project_root, None);

    fs::write(workspace.join("orphan.rs"), "fn orphan() {}\n").unwrap();
    let orphan = fs::canonicalize(workspace.join("orphan.rs")).unwrap();
    service
        .resolve(
            orphan,
            workspace,
            9,
            vec!["Cargo.toml".into()],
            ProjectRootStrategy::Go,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let resolution = loop {
        if let Some(result) = service.try_result() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "project-root worker timed out");
        std::thread::yield_now();
    };
    assert_eq!(resolution.project_root, None);
}

#[test]
fn a_file_not_saved_yet_still_finds_its_project_root() {
    let workspace = tempdir().unwrap();
    let workspace = fs::canonicalize(workspace.path()).unwrap();
    fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let mut service = ProjectRootService::default();
    service
        .resolve(
            workspace.join("src/new.rs"),
            workspace.clone(),
            1,
            vec!["Cargo.toml".into()],
            ProjectRootStrategy::Cargo,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let resolution = loop {
        if let Some(result) = service.try_result() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "project-root worker timed out");
        std::thread::yield_now();
    };
    assert_eq!(resolution.document, workspace.join("src/new.rs"));
    assert_eq!(resolution.project_root, Some(workspace.clone()));
}

#[test]
fn project_roots_accept_jsonc_configs_and_whitespace_separated_go_directives() {
    use super::project_root::project_root_is_valid;
    let root = tempdir().unwrap();
    for marker in ["tsconfig.json", "jsconfig.json"] {
        fs::write(
            root.path().join(marker),
            r#"{
            // TypeScript permits comments and trailing commas.
            "compilerOptions": { "paths": ["https://example/*",], }, /* comment */
        }"#,
        )
        .unwrap();
        assert!(project_root_is_valid(
            root.path(),
            &[marker.into()],
            ProjectRootStrategy::Node
        ));
        for invalid in ["/*", r#"{ "x": 1 /* unterminated }"#, "{,}"] {
            fs::write(root.path().join(marker), invalid).unwrap();
            assert!(!project_root_is_valid(
                root.path(),
                &[marker.into()],
                ProjectRootStrategy::Node
            ));
        }
    }
    fs::write(
        root.path().join("package.json"),
        "{ /* comment */ \"name\": \"demo\", }",
    )
    .unwrap();
    assert!(!project_root_is_valid(
        root.path(),
        &["package.json".into()],
        ProjectRootStrategy::Node
    ));
    for (marker, source) in [
        ("go.mod", "module\texample.com/demo\n"),
        ("go.work", "go\t1.22\nuse ./demo\n"),
    ] {
        fs::write(root.path().join(marker), source).unwrap();
        assert!(project_root_is_valid(
            root.path(),
            &[marker.into()],
            ProjectRootStrategy::Go
        ));
    }
}

#[test]
fn explorer_watch_detects_ignore_rule_edits_in_parent_directories() {
    let root = tempdir().unwrap();
    let child = root.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::write(root.path().join(".gitignore"), "one.txt\n").unwrap();
    let roots = vec![root.path().to_path_buf()];
    let before = explorer_directory_fingerprint(&child, &roots, 10, 4096).unwrap();
    // Same length and no entry-name changes, including in the child listing.
    fs::write(root.path().join(".gitignore"), "two.txt\n").unwrap();
    let after = explorer_directory_fingerprint(&child, &roots, 10, 4096).unwrap();
    assert_eq!(before.entries, after.entries);
    assert_ne!(before, after);
    fs::write(root.path().join(".ignore"), "three.txt\n").unwrap();
    assert_ne!(
        after,
        explorer_directory_fingerprint(&child, &roots, 10, 4096).unwrap()
    );
}

#[test]
fn explorer_watch_fingerprints_are_deterministic_metadata_only_and_bounded() {
    let root = tempdir().unwrap();
    let canonical = fs::canonicalize(root.path()).unwrap();
    fs::write(root.path().join("b"), "one").unwrap();
    fs::create_dir(root.path().join("a")).unwrap();
    let first =
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
            .unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
    fs::write(
        root.path().join("b"),
        "content changes are intentionally ignored",
    )
    .unwrap();
    let unchanged =
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
            .unwrap();
    assert_eq!(first, unchanged);
    fs::rename(root.path().join("b"), root.path().join("c")).unwrap();
    let renamed =
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
            .unwrap();
    let previous = BTreeMap::from([(canonical.clone(), first)]);
    let current = BTreeMap::from([(canonical.clone(), renamed)]);
    assert_eq!(
        diff_explorer_fingerprints(&previous, &current, 1).unwrap(),
        vec![canonical.clone()]
    );
    assert_eq!(
        diff_explorer_fingerprints(&previous, &current, 0),
        Err(ExplorerWatchError::Limit)
    );
    fs::remove_file(root.path().join("c")).unwrap();
    fs::create_dir(root.path().join("c")).unwrap();
    let type_changed =
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
            .unwrap();
    assert_ne!(current[&canonical], type_changed);
    assert_eq!(
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 1, 1024),
        Err(ExplorerWatchError::Limit)
    );
}

#[cfg(unix)]
#[test]
fn explorer_watch_preserves_non_utf8_names_and_never_follows_symlinks() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let root = tempdir().unwrap();
    let canonical = fs::canonicalize(root.path()).unwrap();
    let non_utf8 = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
    assert_eq!(watch_name_bytes(&non_utf8), vec![b'x', 0xff]);
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("hidden"), "x").unwrap();
    symlink(outside.path(), root.path().join("link")).unwrap();
    let snapshot =
        explorer_directory_fingerprint(root.path(), std::slice::from_ref(&canonical), 8, 1024)
            .unwrap();
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.name == b"link" && entry.kind == ExplorerWatchEntryKind::Symlink)
    );
    assert!(!snapshot.entries.iter().any(|entry| entry.name == b"hidden"));
}

#[test]
fn disabled_explorer_watch_has_zero_worker_or_snapshot_activity() {
    let policy = ExplorerWatchPolicy {
        enabled: false,
        poll_interval: Duration::from_millis(50),
        debounce: Duration::ZERO,
        maximum_directories: 2,
        maximum_entries_per_directory: 2,
        maximum_events: 2,
        maximum_path_bytes: 256,
        queue_capacity: 1,
    };
    let mut service = ExplorerWatchService::start(policy).unwrap();
    assert_eq!(service.reconcile(1, Vec::new(), Vec::new()).unwrap(), None);
    assert_eq!(service.reconcile_count(), 0);
    assert!(service.poll().is_empty());
}

#[test]
fn atomic_write_replaces_and_preserves_text_format_bytes() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("document.txt");
    fs::write(&path, b"old").expect("seed");
    let buffer = TextBuffer::from_bytes(b"\xEF\xBB\xBFone\r\ntwo\r\n").expect("decode");
    atomic_write(&path, &buffer.to_bytes()).expect("atomic write");
    assert_eq!(
        fs::read(&path).expect("read"),
        b"\xEF\xBB\xBFone\r\ntwo\r\n"
    );
    let names: Vec<_> = fs::read_dir(root.path())
        .expect("directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, vec!["document.txt"]);
}

#[test]
fn checked_write_detects_external_change_and_save_as_collision() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("checked.txt");
    fs::write(&path, b"initial").expect("seed");
    assert!(matches!(
        checked_atomic_write(
            &path,
            b"editor",
            &WriteExpectation::Exact(b"different".to_vec())
        ),
        Err(VfsError::ExternalModification(_))
    ));
    assert_eq!(fs::read(&path).expect("unchanged"), b"initial");
    assert!(matches!(
        checked_atomic_write(&path, b"editor", &WriteExpectation::Missing),
        Err(VfsError::DestinationExists(_))
    ));
    checked_atomic_write(
        &path,
        b"editor",
        &WriteExpectation::Exact(b"initial".to_vec()),
    )
    .expect("matching write");
    assert_eq!(fs::read(path).expect("written"), b"editor");
}

#[test]
fn recovery_journals_are_versioned_atomic_and_corruption_tolerant() {
    let root = tempdir().expect("tempdir");
    let valid = root.path().join("buffer-a.toml");
    let source = root.path().join("source.rs");
    fs::write(&source, b"source stays intact").expect("source");
    let record = RecoveryRecord::new(Some(&source), 42, "unsaved text".into());
    write_recovery(&valid, &record).expect("journal");
    fs::write(root.path().join("broken.toml"), b"not = [valid").expect("broken");
    let discovery = discover_recovery(root.path());
    assert_eq!(discovery.records, vec![(valid, record)]);
    assert_eq!(discovery.diagnostics.len(), 1);
    assert_eq!(fs::read(source).expect("source"), b"source stays intact");
}

#[test]
fn recovery_paths_round_trip_native_bytes_and_read_legacy_journals() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempdir().unwrap();
    let source = root
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
    let replacement = root.path().join("x\u{fffd}");
    let record = RecoveryRecord::new(Some(&source), 1, "unsaved".into());
    write_recovery(&root.path().join("native.toml"), &record).unwrap();
    let discovery = discover_recovery(root.path());
    assert_eq!(discovery.records[0].1.path(), Some(source));
    assert_ne!(discovery.records[0].1.path(), Some(replacement));
    let legacy = RecoveryRecord {
        version: 1,
        original_path: Some("old.txt".into()),
        revision: 0,
        content: "old".into(),
    };
    fs::write(
        root.path().join("legacy.toml"),
        toml::to_string(&legacy).unwrap(),
    )
    .unwrap();
    let discovery = discover_recovery(root.path());
    assert_eq!(discovery.records.len(), 2);
    assert_eq!(discovery.records[0].1.path(), Some("old.txt".into()));
    let ambiguous = RecoveryRecord {
        original_path: Some("x\u{fffd}".into()),
        ..legacy
    };
    assert!(ambiguous.path().is_none());
    fs::write(
        root.path().join("ambiguous.toml"),
        toml::to_string(&ambiguous).unwrap(),
    )
    .unwrap();
    assert_eq!(discover_recovery(root.path()).records.len(), 3);
    assert!(super::recovery_size_allowed(
        RECOVERY_PAYLOAD_BYTE_CEILING - 8192
    ));
    assert!(!super::recovery_size_allowed(
        RECOVERY_PAYLOAD_BYTE_CEILING - 8191
    ));
}

#[test]
fn recovery_safety_ceilings_are_testable_without_large_disk_fixtures() {
    assert!(recovery_journal_size_allowed(RECOVERY_JOURNAL_BYTE_CEILING));
    assert!(!recovery_journal_size_allowed(
        RECOVERY_JOURNAL_BYTE_CEILING + 1
    ));
    assert!(recovery_payload_size_allowed(
        RECOVERY_PAYLOAD_BYTE_CEILING,
        0
    ));
    assert!(!recovery_payload_size_allowed(
        RECOVERY_PAYLOAD_BYTE_CEILING,
        1
    ));
    let (paths, truncated) = bounded_recovery_entries(
        (0..=RECOVERY_DIRECTORY_ENTRY_CEILING)
            .map(|index| std::path::PathBuf::from(format!("non-journal-{index}"))),
    );
    assert!(truncated);
    assert_eq!(paths.len(), RECOVERY_DIRECTORY_ENTRY_CEILING);
    assert!(
        paths.iter().all(|path| path.extension().is_none()),
        "raw non-TOML entries consume the same bounded scan budget"
    );
}

#[test]
fn document_reader_accepts_exact_limit_and_rejects_limit_plus_one() {
    assert_eq!(
        read_bounded_bytes(std::io::Cursor::new(b"1234"), 4).expect("read"),
        Some(b"1234".to_vec())
    );
    assert_eq!(
        read_bounded_bytes(std::io::Cursor::new(b"12345"), 4).expect("read"),
        None
    );
}

#[test]
fn recovery_reader_enforces_bytes_and_utf8_from_the_open_stream() {
    let exact =
        read_bounded_utf8(std::io::Cursor::new(b"1234"), 4, "test journal").expect("exact limit");
    assert_eq!(exact, "1234");
    let overflow = read_bounded_utf8(std::io::Cursor::new(b"12345"), 4, "test journal")
        .expect_err("limit plus one");
    assert_eq!(overflow, "test journal exceeds 4 byte safety ceiling");
    let invalid = read_bounded_utf8(std::io::Cursor::new([0xff]), 4, "test journal")
        .expect_err("invalid UTF-8");
    assert!(invalid.starts_with("test journal is not UTF-8:"));

    let root = tempdir().expect("tempdir");
    let path = root.path().join("invalid.toml");
    fs::write(&path, [0xff]).expect("invalid journal");
    let discovery = discover_recovery(root.path());
    assert!(discovery.records.is_empty());
    assert!(discovery.diagnostics[0].contains("recovery journal is not UTF-8"));
    let cleanup = cleanup_recovery(root.path(), 0, 0, crate::clock::system_time());
    assert!(cleanup.deleted.is_empty());
    assert!(
        path.exists(),
        "invalid journals remain available for inspection"
    );
}

#[test]
fn recovery_discovery_bounds_records_and_aggregate_content() {
    let root = tempdir().unwrap();
    for name in ["a", "b", "c"] {
        write_recovery(
            &root.path().join(format!("{name}.toml")),
            &RecoveryRecord::new(None, 1, "1234".into()),
        )
        .unwrap();
    }
    let limited = super::recovery::discover_recovery_bounded(root.path(), 1, 100);
    assert_eq!(limited.records.len(), 1);
    let limited = super::recovery::discover_recovery_bounded(root.path(), 100, 8);
    assert_eq!(limited.records.len(), 2);
    assert_eq!(limited.diagnostics.len(), 1);
    assert!(limited.diagnostics[0].contains("byte safety ceiling"));
    assert!(
        super::recovery::discover_recovery_bounded(root.path(), 0, 100)
            .records
            .is_empty()
    );
    let cleanup = cleanup_recovery(root.path(), 0, 0, crate::clock::system_time());
    assert_eq!(cleanup.deleted.len(), 3);
}

#[test]
fn recovery_cleanup_obeys_count_and_age_without_deleting_corruption() {
    let root = tempdir().expect("tempdir");
    let first = root.path().join("a.toml");
    let second = root.path().join("b.toml");
    let corrupt = root.path().join("corrupt.toml");
    write_recovery(&first, &RecoveryRecord::new(None, 1, "one".into())).expect("first");
    write_recovery(&second, &RecoveryRecord::new(None, 2, "two".into())).expect("second");
    fs::write(&corrupt, "broken=[").expect("corrupt");

    let cleanup = cleanup_recovery(root.path(), 1, 0, crate::clock::system_time());
    assert_eq!(cleanup.deleted.len(), 1);
    assert!(
        corrupt.exists(),
        "corrupt journals require explicit inspection"
    );
    assert_eq!(discover_recovery(root.path()).records.len(), 1);

    let future = crate::clock::system_time() + Duration::from_secs(2 * 86_400);
    let cleanup = cleanup_recovery(root.path(), 10, 1, future);
    assert_eq!(cleanup.deleted.len(), 1, "age policy removes the survivor");
    assert!(corrupt.exists());
}

#[test]
fn recovery_worker_fifo_order_makes_delete_the_revision_barrier() {
    let root = tempdir().expect("tempdir");
    let journal = root.path().join("buffer.toml");
    let mut worker = VfsWorker::start(4);
    fn expect_completion(worker: &mut VfsWorker, id: super::RequestId, path: &Path, write: bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(reply) = worker.try_result().expect("worker") {
                let (actual_id, actual_path, result) = match reply {
                    VfsResult::RecoveryWritten { id, path, result } if write => (id, path, result),
                    VfsResult::Deleted { id, path, result } if !write => (id, path, result),
                    reply => panic!("unexpected recovery completion: {reply:?}"),
                };
                assert_eq!(actual_id, id);
                assert_eq!(actual_path, path);
                result.expect("recovery operation succeeds");
                return;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let write = worker
        .write_recovery(journal.clone(), RecoveryRecord::new(None, 1, "old".into()))
        .expect("queue old write");
    let delete = worker.delete(journal.clone()).expect("queue delete");
    expect_completion(&mut worker, write, &journal, true);
    expect_completion(&mut worker, delete, &journal, false);
    assert!(
        !journal.exists(),
        "old write followed by delete stays deleted"
    );

    let delete = worker.delete(journal.clone()).expect("queue barrier");
    let write = worker
        .write_recovery(journal.clone(), RecoveryRecord::new(None, 2, "new".into()))
        .expect("queue new write");
    expect_completion(&mut worker, delete, &journal, false);
    expect_completion(&mut worker, write, &journal, true);
    let recovered = discover_recovery(root.path());
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, 2);
    assert_eq!(recovered.records[0].1.content, "new");
}

// Re-execute only this test with private Git configuration; never mutate the parallel
// test runner's environment while filesystem workers are running.
pub(super) fn isolated_git_test() -> bool {
    let thread = std::thread::current();
    let name = thread.name().expect("test name");
    if std::env::var("DUST_ISOLATED_GIT_TEST").as_deref() == Ok(name) {
        return true;
    }
    let home = tempdir().expect("test home");
    let config = home.path().join(".gitconfig");
    fs::write(&config, "").expect("empty Git config");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name, "--nocapture"])
        .env("DUST_ISOLATED_GIT_TEST", name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_SYSTEM", &config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("isolated filesystem test");
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    false
}

pub(super) fn search_policy(root: &Path, query: &str) -> WorkspaceSearchPolicy {
    WorkspaceSearchPolicy {
        roots: vec![root.to_path_buf()],
        query: query.into(),
        regex: false,
        smart_case: true,
        whole_word: false,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        include_hidden: false,
        include_ignored: false,
        maximum_files: 1_000,
        maximum_results: 1_000,
        maximum_file_bytes: 1024 * 1024,
        maximum_total_bytes: 8 * 1024 * 1024,
        maximum_preview_bytes: 4_096,
        batch_size: 2,
        event_queue_capacity: 2,
    }
}

pub(super) fn collect_search(
    service: &WorkspaceSearchService,
) -> (Vec<WorkspaceSearchMatch>, WorkspaceSearchSummary) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut matches = Vec::new();
    loop {
        if let Some(event) = service.try_event() {
            match event {
                WorkspaceSearchEvent::Batch { matches: batch, .. } => matches.extend(batch),
                WorkspaceSearchEvent::Complete { summary, .. } => return (matches, summary),
                WorkspaceSearchEvent::Failed { error, .. } => panic!("search failed: {error}"),
                WorkspaceSearchEvent::Cancelled { .. } => panic!("search cancelled"),
            }
        }
        assert!(Instant::now() < deadline, "search timed out");
        std::thread::yield_now();
    }
}

fn collect_files(policy: super::WorkspaceFilePolicy) -> (Vec<super::WorkspaceFileEntry>, super::WorkspaceFileSummary) {
    let mut service = super::WorkspaceFileService::default();
    let generation = service.start(policy.clone()).expect("start finder");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut entries = Vec::new();
    loop {
        match service.try_event() {
            Some(super::WorkspaceFileEvent::Batch { generation: actual, root_revision, entries: batch }) => {
                assert_eq!((actual, root_revision), (generation, policy.root_revision));
                entries.extend(batch);
            }
            Some(super::WorkspaceFileEvent::Complete { generation: actual, root_revision, summary }) => {
                assert_eq!((actual, root_revision), (generation, policy.root_revision));
                return (entries, summary);
            }
            Some(event) => panic!("finder failed: {event:?}"),
            None => {}
        }
        assert!(Instant::now() < deadline, "finder timed out");
        std::thread::yield_now();
    }
}

/// A repository root that Git told to ignore `.worktree/`, with one linked worktree in it.
fn repository_with_worktree() -> tempfile::TempDir {
    let root = tempdir().expect("root");
    fs::create_dir_all(root.path().join(".git/info")).expect("git");
    fs::write(root.path().join(".git/info/exclude"), "/.worktree/\n").expect("exclude");
    fs::write(root.path().join("main.txt"), "needle\n").expect("main");
    let worktree = root.path().join(".worktree/feat");
    fs::create_dir_all(&worktree).expect("worktree");
    let link = format!("gitdir: {}/.git/worktrees/feat\n", root.path().display());
    fs::write(worktree.join(".git"), link).expect("link");
    fs::write(worktree.join(".gitignore"), "built.txt\n").expect("ignore");
    fs::write(worktree.join("feat.txt"), "needle\n").expect("feat");
    fs::write(worktree.join("built.txt"), "needle\n").expect("built");
    root
}

#[test]
fn a_root_shows_its_worktrees_and_a_worktree_shows_only_itself() {
    if !isolated_git_test() { return; }
    let repository = repository_with_worktree();
    let root = repository.path().canonicalize().expect("root");
    let worktree = root.join(".worktree/feat");

    let listing = list_scoped_directory(1, 0, &root, std::slice::from_ref(&root), false, false, 10)
        .expect("root listing");
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| entry.name == ".worktree")
    );
    let inside = root.join(".worktree");
    let listing =
        list_scoped_directory(1, 0, &inside, std::slice::from_ref(&root), false, false, 10)
            .expect("worktree folder listing");
    assert!(listing.entries.iter().any(|entry| entry.name == "feat"));

    let searched = |root: &Path| {
        let mut service = WorkspaceSearchService::default();
        service.start(search_policy(root, "needle")).expect("start");
        let mut paths = collect_search(&service)
            .0
            .into_iter()
            .map(|found| found.relative_path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    };
    assert_eq!(searched(&root), [".worktree/feat/feat.txt", "main.txt"]);
    assert_eq!(searched(&worktree), ["feat.txt"]);

    let found = |root: &Path| {
        let mut service = super::WorkspaceFileService::default();
        service
            .start(super::WorkspaceFilePolicy {
                roots: vec![root.to_path_buf()],
                root_revision: 1,
                include_hidden: false,
                include_ignored: false,
                maximum_files: 10,
                maximum_path_bytes: 4096,
                batch_size: 8,
                event_queue_capacity: 8,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut entries = Vec::new();
        loop {
            match service.try_event() {
                Some(super::WorkspaceFileEvent::Batch { entries: batch, .. }) => {
                    entries.extend(batch)
                }
                Some(super::WorkspaceFileEvent::Complete { .. }) => break,
                Some(super::WorkspaceFileEvent::Failed { error, .. }) => panic!("{error}"),
                _ => {}
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        entries
            .iter()
            .map(|entry: &super::WorkspaceFileEntry| {
                entry.relative_path.to_string_lossy().into_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(found(&root), [".worktree/feat/feat.txt", "main.txt"]);
    assert_eq!(found(&worktree), ["feat.txt"]);

    assert_eq!(
        super::linked_worktree_repository(&worktree).as_deref(),
        root.file_name().and_then(|name| name.to_str())
    );
    assert_eq!(super::linked_worktree_repository(&root), None);
    fs::write(worktree.join(".git"), "gitdir: ../../.git/worktrees/feat\n").unwrap();
    assert_eq!(
        super::linked_worktree_repository(&worktree).as_deref(),
        root.file_name().and_then(|name| name.to_str())
    );
}

#[test]
fn root_worktree_discovery_is_lazy_bounded_and_cancellable() {
    let root = tempdir().unwrap();
    for index in 0..20 {
        let worktree = root.path().join(format!(".worktree/{index}"));
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../../.git/worktrees/feat").unwrap();
    }
    let mut scanned = 0;
    assert!(super::directory::root_worktrees(root.path(), || { scanned += 1; true }).next().is_some());
    assert_eq!(scanned, 1, "asking for one worktree inspects only one candidate");
    assert_eq!(super::directory::root_worktrees(root.path(), || false).count(), 0);
    let mut remaining = 2;
    let paths = super::directory::root_worktrees(root.path(), || {
        if remaining == 0 { return false; }
        remaining -= 1;
        true
    }).collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
}

#[test]
fn worktree_discovery_and_special_directory_respect_caller_limits() {
    if !isolated_git_test() { return; }
    let repository = repository_with_worktree();
    let root = repository.path().canonicalize().unwrap();
    let listing = list_scoped_directory(1, 0, &root, std::slice::from_ref(&root), false, false, 1).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.truncated, "the extra worktree entry did not fit");
    fs::create_dir_all(root.join(".worktree/other")).unwrap();
    fs::remove_file(root.join(".worktree/feat/feat.txt")).unwrap();
    // Only main.txt is visible, so truncation comes from discovery, not file collection.
    // Even non-worktree children consume the discovery budget.
    let (entries, summary) = collect_files(super::WorkspaceFilePolicy {
        roots: vec![root.clone()], root_revision: 1,
        include_hidden: false, include_ignored: false,
        maximum_files: 1, maximum_path_bytes: 4096,
        batch_size: 1, event_queue_capacity: 1,
    });
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].relative_path, Path::new("main.txt"));
    assert!(summary.truncated);
    let mut policy = search_policy(&root, "needle");
    policy.maximum_files = 1;
    let mut search = WorkspaceSearchService::default();
    search.start(policy).unwrap();
    let (matches, summary) = collect_search(&search);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].relative_path, Path::new("main.txt"));
    assert!(summary.truncated);
}

#[cfg(unix)]
#[test]
fn worktree_symlinks_cannot_add_outside_files_to_finder_or_grep() {
    if !isolated_git_test() { return; }
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let outside = repository_with_worktree();
    let worktrees = root.path().join(".worktree");
    symlink(outside.path().join(".worktree"), &worktrees).unwrap();
    for linked_directory in [true, false] {
        if !linked_directory {
            fs::remove_file(&worktrees).unwrap();
            fs::create_dir(&worktrees).unwrap();
            symlink(outside.path().join(".worktree/feat"), worktrees.join("feat")).unwrap();
        }
        assert_eq!(super::directory::root_worktrees(root.path(), || true).count(), 0);
        let (entries, summary) = collect_files(super::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf()], root_revision: 1,
            include_hidden: false, include_ignored: false,
            maximum_files: 10, maximum_path_bytes: 4096,
            batch_size: 1, event_queue_capacity: 1,
        });
        assert!(entries.is_empty());
        assert!(!summary.truncated);
        let mut search = WorkspaceSearchService::default();
        search.start(search_policy(root.path(), "needle")).unwrap();
        assert!(collect_search(&search).0.is_empty());
    }
}

#[test]
fn explorer_natural_key_is_case_folded_numeric_and_stably_zero_aware() {
    let mut names = vec!["file10", "File2", "file01", "file1", "file02"];
    names.sort_by(|left, right| {
        natural_name_key(left)
            .cmp(&natural_name_key(right))
            .then_with(|| left.cmp(right))
    });
    assert_eq!(names, ["file1", "file01", "File2", "file02", "file10"]);
}

#[test]
fn scoped_directory_listing_is_lazy_sorted_hidden_aware_and_bounded() {
    if !isolated_git_test() { return; }
    let root = tempdir().expect("root");
    fs::create_dir(root.path().join("z-dir")).expect("dir");
    fs::create_dir(root.path().join("dir10")).expect("dir10");
    fs::create_dir(root.path().join("dir2")).expect("dir2");
    fs::write(root.path().join("a-file"), "a").expect("file");
    fs::write(root.path().join("file10"), "10").expect("file10");
    fs::write(root.path().join("file2"), "2").expect("file2");
    fs::write(root.path().join(".hidden"), "h").expect("hidden");
    let canonical = fs::canonicalize(root.path()).expect("canonical");
    let mut service = DirectoryService::default();
    service
        .list(
            canonical.clone(),
            vec![canonical.clone()],
            7,
            false,
            false,
            10,
        )
        .expect("list");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = service.try_result() {
            let listing = result.expect("listing");
            assert_eq!(listing.root_revision, 7);
            let ordered = listing
                .entries
                .iter()
                .map(|entry| (entry.kind, entry.name.as_str()))
                .collect::<Vec<_>>();
            assert_eq!(
                ordered,
                [
                    (DirectoryEntryKind::Directory, "dir2"),
                    (DirectoryEntryKind::Directory, "dir10"),
                    (DirectoryEntryKind::Directory, "z-dir"),
                    (DirectoryEntryKind::File, "a-file"),
                    (DirectoryEntryKind::File, "file2"),
                    (DirectoryEntryKind::File, "file10"),
                ]
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[cfg(unix)]
#[test]
fn scoped_directory_listing_does_not_traverse_symlink_escape_and_handles_non_utf8_name() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let root = tempdir().expect("root");
    let outside = tempdir().expect("outside");
    symlink(outside.path(), root.path().join("escape")).expect("symlink");
    let invalid = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
    let invalid_created = fs::write(root.path().join(invalid), "x").is_ok();
    let canonical = fs::canonicalize(root.path()).expect("canonical");
    let result = list_scoped_directory(
        1,
        0,
        &canonical,
        std::slice::from_ref(&canonical),
        true,
        true,
        10,
    )
    .expect("list");
    assert!(
        result
            .entries
            .iter()
            .any(|entry| entry.kind == DirectoryEntryKind::Symlink)
    );
    if invalid_created {
        assert!(result.entries.iter().any(|entry| entry.name.contains('�')));
    }
    assert!(matches!(
        list_scoped_directory(
            2,
            0,
            outside.path(),
            std::slice::from_ref(&canonical),
            true,
            true,
            10
        ),
        Err(VfsError::OutsideScope(_))
    ));
}

#[test]
fn workspace_files_and_search_keep_distinct_non_utf8_path_identities() {
    if !isolated_git_test() { return; }
    use std::os::unix::ffi::OsStringExt;
    let root = tempdir().unwrap();
    let paths = [0xfe, 0xff].map(|byte| {
        root.path()
            .join(std::ffi::OsString::from_vec(vec![b'x', byte]))
    });
    assert_eq!(
        paths[0].display().to_string(),
        paths[1].display().to_string()
    );
    assert_ne!(
        super::workspace_files::file_id(&paths[0]),
        super::workspace_files::file_id(&paths[1])
    );
    for path in paths {
        if let Err(error) = fs::write(&path, "needle") {
            // Some filesystems require UTF-8 names; the identity checks above still apply.
            assert_eq!(error.raw_os_error(), Some(nix::libc::EILSEQ));
            return;
        }
    }
    let mut service = super::WorkspaceFileService::default();
    service
        .start(super::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf()],
            root_revision: 1,
            include_hidden: true,
            include_ignored: true,
            maximum_files: 10,
            maximum_path_bytes: 4096,
            batch_size: 10,
            event_queue_capacity: 8,
        })
        .unwrap();
    let mut entries = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match service.try_event() {
            Some(super::WorkspaceFileEvent::Batch { entries: batch, .. }) => entries.extend(batch),
            Some(super::WorkspaceFileEvent::Complete { .. }) => break,
            Some(super::WorkspaceFileEvent::Failed { error, .. }) => panic!("{error}"),
            _ => {}
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].relative_path.display().to_string(),
        entries[1].relative_path.display().to_string()
    );
    assert_ne!(entries[0].id, entries[1].id);
    let mut search = WorkspaceSearchService::default();
    search.start(search_policy(root.path(), "needle")).unwrap();
    let (matches, _) = collect_search(&search);
    assert_eq!(matches.len(), 2);
    assert_ne!(matches[0].id, matches[1].id);
}

#[test]
fn workspace_search_positions_many_matches_after_a_large_prefix() {
    if !isolated_git_test() { return; }
    let root = tempdir().unwrap();
    let prefix = 7 * 1024 * 1024;
    fs::write(
        root.path().join("large.txt"),
        format!("{}{}\nend a", "x".repeat(prefix), "a".repeat(10_000)),
    )
    .unwrap();
    let mut policy = search_policy(root.path(), "a");
    policy.maximum_file_bytes = 8 * 1024 * 1024;
    policy.maximum_total_bytes = 8 * 1024 * 1024;
    policy.maximum_results = 10_001;
    let mut service = WorkspaceSearchService::default();
    service.start(policy).unwrap();
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 10_001);
    for (index, found) in matches[..10_000].iter().enumerate() {
        assert_eq!(
            (found.line, found.column, found.end_line, found.end_column),
            (0, prefix + index, 0, prefix + index + 1)
        );
    }
    assert_eq!((matches[10_000].line, matches[10_000].column), (1, 4));
}

#[test]
fn workspace_search_is_unicode_exact_regex_smart_case_and_whole_word() {
    if !isolated_git_test() { return; }
    let root = tempdir().expect("root");
    fs::write(root.path().join("a.txt"), "café CAFE cat scatter cat\n").expect("a");
    fs::write(root.path().join("bom.txt"), b"\xef\xbb\xbfcat\r\n").expect("bom");
    let mut service = WorkspaceSearchService::default();
    let mut policy = search_policy(root.path(), "café|cat");
    policy.regex = true;
    policy.whole_word = true;
    service.start(policy).expect("start");
    let (matches, summary) = collect_search(&service);
    assert_eq!(matches.len(), 4);
    assert_eq!(
        &matches[0].line_text[matches[0].column..matches[0].range.end],
        "café"
    );
    let bom = matches
        .iter()
        .find(|item| item.relative_path == Path::new("bom.txt"))
        .expect("BOM match");
    assert_eq!(bom.range, 0..3);
    assert_eq!(summary.files_searched, 2);

    let mut policy = search_policy(root.path(), "CAFE");
    policy.smart_case = true;
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1, "uppercase smart-case query is sensitive");
}

#[test]
fn workspace_search_obeys_hidden_ignore_globs_binary_and_limits() {
    if !isolated_git_test() { return; }
    let root = tempdir().expect("root");
    fs::create_dir(root.path().join(".git")).expect("git");
    fs::write(root.path().join(".gitignore"), "ignored.txt\n").expect("ignore");
    fs::write(root.path().join("visible.rs"), "needle\n").expect("visible");
    fs::write(root.path().join("ignored.txt"), "needle\n").expect("ignored");
    fs::write(root.path().join(".hidden.rs"), "needle\n").expect("hidden");
    fs::write(root.path().join("binary.rs"), b"needle\0binary").expect("binary");
    fs::write(root.path().join("invalid.rs"), b"needle\xff").expect("invalid UTF-8");
    fs::write(root.path().join("other.txt"), "needle\n").expect("other");
    let mut service = WorkspaceSearchService::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.include_globs = vec!["**/*.rs".into()];
    policy.maximum_results = 1;
    service.start(policy).expect("start");
    let (matches, summary) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].relative_path, Path::new("visible.rs"));
    assert_eq!(summary.skipped_binary, 2);
    assert!(!summary.truncated, "the only textual included match fit");

    let mut policy = search_policy(root.path(), "needle");
    policy.include_hidden = true;
    policy.include_ignored = true;
    policy.exclude_globs = vec!["**/*.txt".into()];
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 2);
    assert!(
        matches
            .iter()
            .any(|item| item.relative_path == Path::new(".hidden.rs"))
    );
}

#[test]
fn workspace_search_bounds_picker_preview_text() {
    if !isolated_git_test() { return; }
    let root = tempdir().expect("root");
    fs::write(
        root.path().join("long"),
        format!("needle{}", "x".repeat(10_000)),
    )
    .expect("long line");
    let mut service = WorkspaceSearchService::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.maximum_preview_bytes = 16;
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert!(matches[0].line_text.len() <= 16);
}

#[test]
fn workspace_search_obeys_per_file_and_aggregate_byte_ceilings() {
    if !isolated_git_test() { return; }
    let root = tempdir().expect("root");
    fs::write(root.path().join("a"), "needle plus").expect("a");
    fs::write(root.path().join("b"), "needle plus").expect("b");
    let mut service = WorkspaceSearchService::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.maximum_file_bytes = 8;
    service.start(policy).expect("start");
    let (matches, summary) = collect_search(&service);
    assert!(matches.is_empty());
    assert_eq!(summary.skipped_too_large, 2);

    let mut policy = search_policy(root.path(), "needle");
    policy.maximum_total_bytes = 12;
    service.start(policy).expect("start");
    let (matches, summary) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert!(summary.truncated);
}

#[cfg(unix)]
#[test]
fn workspace_search_does_not_follow_symlink_escapes_and_nested_roots_deduplicate() {
    if !isolated_git_test() { return; }
    use std::os::unix::fs::symlink;
    let root = tempdir().expect("root");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested");
    fs::write(nested.join("inside"), "needle").expect("inside");
    let outside = tempdir().expect("outside");
    fs::write(outside.path().join("secret"), "needle").expect("secret");
    symlink(outside.path(), root.path().join("escape")).expect("link");
    let mut service = WorkspaceSearchService::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.roots.push(nested.clone());
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].root, nested);
}

#[cfg(unix)]
#[test]
fn scoped_read_rejects_symlink_escape_after_canonicalization() {
    use std::os::unix::fs::symlink;
    let root = tempdir().expect("root");
    let outside = tempdir().expect("outside");
    let secret = outside.path().join("secret");
    fs::write(&secret, "secret").expect("secret");
    let link = root.path().join("link");
    symlink(&secret, &link).expect("symlink");
    let mut worker = VfsWorker::start(2);
    let id = worker
        .read_scoped(
            link,
            vec![fs::canonicalize(root.path()).expect("canonical root")],
        )
        .expect("queue");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Read {
            id: result_id,
            result,
            ..
        }) = worker.try_result().expect("result")
            && result_id == id
        {
            assert!(matches!(result, Err(VfsError::OutsideScope(_))));
            break;
        }
        assert!(Instant::now() < deadline, "scoped read timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn directory_completion_is_bounded_and_runs_on_worker() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("b.txt"), b"").expect("b");
    fs::write(root.path().join("a.txt"), b"").expect("a");
    fs::write(root.path().join("c.txt"), b"").expect("c");
    let mut worker = VfsWorker::start(2);
    let id = worker
        .list_directory(root.path().to_path_buf(), String::new(), 2)
        .expect("queue list");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Directory {
            id: result_id,
            result,
            ..
        }) = worker.try_result().expect("worker")
        {
            assert_eq!(result_id, id);
            let paths = result.expect("list");
            assert_eq!(paths.len(), 2);
            assert!(paths[0].ends_with("a.txt"));
            assert!(paths[1].ends_with("b.txt"));
            break;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn directory_completion_filters_names_before_capping_worker_results() {
    let root = tempdir().expect("tempdir");
    let names = (0..1005).map(|index| format!("File-{index:04}.txt")).collect::<Vec<_>>();
    for name in &names {
        fs::write(root.path().join(name), b"").expect("file");
    }
    let initial = super::vfs::list_directory(root.path(), "", 1000).expect("unfiltered listing");
    assert_eq!(initial.len(), 1000);
    let target = names.iter().find(|name| !initial.contains(&root.path().join(name))).unwrap();
    let mut worker = VfsWorker::start(2);
    let id = worker.list_directory(root.path().to_path_buf(), target.to_uppercase(), 1000).expect("queue filtered list");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Directory { id: result_id, result, .. }) = worker.try_result().expect("worker") {
            assert_eq!(result_id, id);
            assert_eq!(result.expect("filtered listing"), vec![root.path().join(target)]);
            break;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn explorer_mutations_are_scoped_non_overwriting_and_worker_owned() {
    fn apply(
        worker: &mut VfsWorker,
        root: &Path,
        operation: ExplorerMutation,
    ) -> Result<(), VfsError> {
        let id = worker
            .explorer_mutate(
                operation,
                vec![fs::canonicalize(root).expect("canonical root")],
                1024,
            )
            .expect("queue mutation");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(VfsResult::ExplorerMutated {
                id: result_id,
                result,
                ..
            }) = worker.try_result().expect("worker")
                && result_id == id
            {
                return result;
            }
            assert!(Instant::now() < deadline, "explorer mutation timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    let root = tempdir().expect("root");
    let mut worker = VfsWorker::start(4);
    let first = root.path().join("first.txt");
    apply(
        &mut worker,
        root.path(),
        ExplorerMutation::Create {
            path: first.clone(),
            directory: false,
        },
    )
    .expect("create");
    fs::write(&first, "content").expect("seed copy source");
    let renamed = root.path().join("renamed.txt");
    apply(
        &mut worker,
        root.path(),
        ExplorerMutation::Rename {
            source: first,
            target: renamed.clone(),
        },
    )
    .expect("rename");
    let copied = root.path().join("copied.txt");
    apply(
        &mut worker,
        root.path(),
        ExplorerMutation::Copy {
            source: renamed.clone(),
            target: copied.clone(),
        },
    )
    .expect("copy");
    assert_eq!(fs::read_to_string(&copied).unwrap(), "content");
    assert!(matches!(
        apply(
            &mut worker,
            root.path(),
            ExplorerMutation::Copy {
                source: renamed.clone(),
                target: copied.clone(),
            }
        ),
        Err(VfsError::DestinationExists(path)) if path == copied
    ));
    apply(
        &mut worker,
        root.path(),
        ExplorerMutation::Delete { path: renamed },
    )
    .expect("delete");
    assert!(!root.path().join("renamed.txt").exists());
}

#[test]
fn worker_reports_write_completion() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("async.txt");
    let mut worker = VfsWorker::start(2);
    let id = worker
        .write(path.clone(), b"saved".to_vec())
        .expect("queue");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Write {
            id: result_id,
            result,
            ..
        }) = worker.try_result().expect("worker")
        {
            assert_eq!(result_id, id);
            result.expect("write");
            break;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(fs::read(path).expect("read"), b"saved");
}
#[test]
fn workspace_files_are_bounded_deterministic_and_ignore_symlink_escapes() {
    if !isolated_git_test() { return; }
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("b.txt"), "b").unwrap();
    fs::write(root.path().join("sub/a.txt"), "a").unwrap();
    fs::write(root.path().join(".hidden"), "h").unwrap();
    #[cfg(unix)]
    let outside = tempdir().unwrap();
    #[cfg(unix)]
    {
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    }
    let mut service = super::WorkspaceFileService::default();
    let generation = service
        .start(super::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf()],
            root_revision: 7,
            include_hidden: false,
            include_ignored: false,
            maximum_files: 10,
            maximum_path_bytes: 4096,
            batch_size: 1,
            event_queue_capacity: 8,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut entries = Vec::new();
    let summary = loop {
        if let Some(event) = service.try_event() {
            match event {
                super::WorkspaceFileEvent::Batch {
                    generation: g,
                    root_revision,
                    entries: batch,
                } => {
                    assert_eq!((g, root_revision), (generation, 7));
                    entries.extend(batch)
                }
                super::WorkspaceFileEvent::Complete { summary, .. } => break summary,
                super::WorkspaceFileEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    };
    assert_eq!(summary.files, 2);
    assert!(!summary.truncated);
    assert_eq!(
        entries
            .iter()
            .map(|e| e.relative_path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["b.txt", "sub/a.txt"]
    );
}

#[test]
fn workspace_file_count_and_path_byte_limits_report_truncation() {
    if !isolated_git_test() { return; }
    let root = tempdir().unwrap();
    for name in ["a", "b", "c"] {
        fs::write(root.path().join(name), "text").unwrap();
    }
    let mut policy = super::WorkspaceFilePolicy {
        roots: vec![root.path().to_path_buf()], root_revision: 7,
        include_hidden: false, include_ignored: false,
        maximum_files: 1, maximum_path_bytes: 4096,
        batch_size: 1, event_queue_capacity: 1,
    };
    let (entries, summary) = collect_files(policy.clone());
    assert_eq!(entries.len(), 1);
    assert_eq!(summary.files, 1);
    assert!(summary.truncated);
    let cost = entries[0].canonical_path.as_os_str().len() + entries[0].relative_path.as_os_str().len();
    assert_eq!(summary.path_bytes, cost);
    policy.maximum_files = 10;
    policy.maximum_path_bytes = cost;
    let (entries, summary) = collect_files(policy.clone());
    assert_eq!(entries.len(), 1, "exactly one path fits the byte budget");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.path_bytes, cost);
    assert!(summary.truncated);
    policy.maximum_path_bytes = cost - 1;
    let (entries, summary) = collect_files(policy);
    assert!(entries.is_empty());
    assert_eq!(summary.files, 0);
    assert_eq!(summary.path_bytes, 0);
    assert!(summary.truncated);
}

#[test]
fn patterned_project_marker_detects_opam_project() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("example.opam"), "opam-version: \"2.0\"").unwrap();
    let result = super::project_root::resolve_project_root(
        1,
        1,
        &root.path().join("main.ml"),
        root.path(),
        &["*.opam".into()],
        ProjectRootStrategy::Marker,
    )
    .unwrap();
    assert_eq!(
        result.project_root,
        Some(root.path().canonicalize().unwrap())
    );
    std::fs::remove_file(root.path().join("example.opam")).unwrap();
    let result = super::project_root::resolve_project_root(
        1,
        1,
        &root.path().join("main.ml"),
        root.path(),
        &["*.opam".into()],
        ProjectRootStrategy::Marker,
    )
    .unwrap();
    assert!(result.project_root.is_none());
}
