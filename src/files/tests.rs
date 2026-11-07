//! Tests for filesystem reads, writes and directory traversal.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crate::editor::text::TextBuffer;
use tempfile::tempdir;

use crate::files::directory::list_scoped_directory;

use crate::files::DirectoryEntryKind;
use crate::files::DirectoryWorker;
use crate::files::ExplorerMutation;
#[cfg(unix)]
use crate::files::ExplorerWatchEntryKind;
use crate::files::ExplorerWatchError;
use crate::files::ExplorerWatchPolicy;
use crate::files::ExplorerWatchWorker;
use crate::files::ProjectRootStrategy;
use crate::files::ProjectRootWorker;
use crate::files::RecoveryRecord;
use crate::files::VfsError;
use crate::files::VfsResult;
use crate::files::VfsWorker;
use crate::files::WorkspaceSearchEvent;
use crate::files::WorkspaceSearchMatch;
use crate::files::WorkspaceSearchPolicy;
use crate::files::WorkspaceSearchSummary;
use crate::files::WorkspaceSearchWorker;
use crate::files::WriteExpectation;
use crate::files::atomic_write;
use crate::files::cleanup_recovery;
use crate::files::diff_explorer_fingerprints;
use crate::files::directory::natural_name_key;
use crate::files::discover_recovery;
use crate::files::explorer_directory_fingerprint;
use crate::files::recovery::RECOVERY_DIRECTORY_ENTRY_LIMIT;
use crate::files::recovery::RECOVERY_JOURNAL_BYTE_LIMIT;
use crate::files::recovery::RECOVERY_PAYLOAD_BYTE_LIMIT;
use crate::files::recovery::bounded_recovery_entries;
use crate::files::recovery::read_bounded_bytes;
use crate::files::recovery::recovery_journal_size_allowed;
use crate::files::recovery::recovery_payload_size_allowed;
use crate::files::vfs::checked_atomic_write;
#[cfg(unix)]
use crate::files::watch::watch_name_bytes;
use crate::files::write_recovery;

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
    let workspace = crate::files::canonicalize(workspace.path()).unwrap();
    let document = crate::files::canonicalize(document).unwrap();
    let mut service = ProjectRootWorker::default();
    let request_id = service
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
    assert_eq!(resolution.request_id, request_id);
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
            crate::files::canonicalize(broken).unwrap(),
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
    let orphan = crate::files::canonicalize(workspace.join("orphan.rs")).unwrap();
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
    let workspace = crate::files::canonicalize(workspace.path()).unwrap();
    fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let mut service = ProjectRootWorker::default();
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

// Creates Unix FIFOs and symlinks.
#[cfg(unix)]
#[test]
fn marker_reads_reject_fifos_symlinks_and_oversized_files() {
    use crate::files::project_root::PROJECT_ROOT_MANIFEST_BYTE_LIMIT;
    use crate::files::project_root::bounded_project_marker;
    let root = tempdir().unwrap();
    let marker = root.path().join("Cargo.toml");
    nix::unistd::mkfifo(&marker, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(bounded_project_marker(root.path(), "Cargo.toml").is_none());
    fs::remove_file(&marker).unwrap();
    let target = root.path().join("manifest");
    fs::write(&target, "[workspace]\n").unwrap();
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    assert!(bounded_project_marker(root.path(), "Cargo.toml").is_none());
    fs::remove_file(&marker).unwrap();
    fs::write(
        &marker,
        "x".repeat(PROJECT_ROOT_MANIFEST_BYTE_LIMIT as usize + 1),
    )
    .unwrap();
    assert!(bounded_project_marker(root.path(), "Cargo.toml").is_none());
    fs::write(&marker, [0xff]).unwrap();
    assert!(bounded_project_marker(root.path(), "Cargo.toml").is_none());
    fs::write(&marker, "[workspace]\n").unwrap();
    assert_eq!(
        bounded_project_marker(root.path(), "Cargo.toml").as_deref(),
        Some("[workspace]\n")
    );
}

#[test]
fn project_roots_accept_jsonc_configs_and_whitespace_separated_go_directives() {
    use crate::files::project_root::project_root_is_valid;
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
fn project_roots_accept_bom_prefixed_node_manifests() {
    let root = tempdir().unwrap();
    for marker in ["tsconfig.json", "jsconfig.json", "package.json"] {
        fs::write(root.path().join(marker), "\u{feff}{\"name\":\"demo\"}").unwrap();
        let resolution = crate::files::project_root::resolve_project_root(
            1,
            1,
            &root.path().join("src/new.ts"),
            root.path(),
            &[marker.into()],
            ProjectRootStrategy::Node,
        )
        .unwrap();
        assert_eq!(
            resolution.project_root,
            Some(crate::files::canonicalize(root.path()).unwrap())
        );
        fs::remove_file(root.path().join(marker)).unwrap();
    }
}

#[test]
fn project_roots_accept_jsonc_comments_with_all_line_endings() {
    use crate::files::project_root::project_root_is_valid;
    let root = tempdir().unwrap();
    for marker in ["tsconfig.json", "jsconfig.json"] {
        for newline in ["\r", "\n", "\r\n"] {
            let source =
                format!("{{{newline}// comment{newline}\"compilerOptions\": {{}},{newline}}}");
            fs::write(root.path().join(marker), source).unwrap();
            assert!(project_root_is_valid(
                root.path(),
                &[marker.into()],
                ProjectRootStrategy::Node
            ));
        }
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

#[cfg(unix)]
#[test]
fn explorer_watch_reads_symlinked_ignore_files() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("ignore");
    fs::write(&target, "one.txt\n").unwrap();
    symlink(&target, root.path().join(".gitignore")).unwrap();
    let roots = vec![crate::files::canonicalize(root.path()).unwrap()];
    let before = explorer_directory_fingerprint(root.path(), &roots, 10, 4096).unwrap();
    fs::write(&target, "two.txt\n").unwrap();
    let after = explorer_directory_fingerprint(root.path(), &roots, 10, 4096).unwrap();
    assert_eq!(before.entries, after.entries);
    assert_ne!(before.ignore_rules, after.ignore_rules);

    // Initial reconciliation must also succeed, not just individual reads.
    let mut watch = ExplorerWatchWorker::start(ExplorerWatchPolicy {
        poll_interval: Duration::from_millis(10),
        debounce: Duration::ZERO,
        ..ExplorerWatchPolicy::default()
    })
    .unwrap();
    watch.reconcile(1, roots.clone(), roots).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut revision = 0;
    loop {
        revision += 1;
        fs::write(&target, format!("{revision}.txt\n")).unwrap();
        let events = watch.poll();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, crate::files::ExplorerWatchEvent::Failed { .. }))
        );
        if events
            .iter()
            .any(|event| matches!(event, crate::files::ExplorerWatchEvent::Batch(_)))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watch did not observe symlink target edits"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn explorer_watch_fingerprints_are_deterministic_metadata_only_and_bounded() {
    let root = tempdir().unwrap();
    let canonical = crate::files::canonicalize(root.path()).unwrap();
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
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let canonical = crate::files::canonicalize(root.path()).unwrap();
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
    let mut service = ExplorerWatchWorker::start(policy).unwrap();
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

// Checks Unix ownership and permission bits.
#[cfg(unix)]
#[test]
fn atomic_write_preserves_destination_ownership_and_permissions() {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::chown;
    let root = tempdir().unwrap();
    let path = root.path().join("shared.txt");
    fs::write(&path, b"before").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let group = if metadata.uid() == 0 {
        metadata.gid().saturating_add(1)
    } else {
        let groups = std::process::Command::new("id").arg("-G").output().unwrap();
        assert!(groups.status.success());
        String::from_utf8(groups.stdout)
            .unwrap()
            .split_whitespace()
            .filter_map(|group| group.parse::<u32>().ok())
            .find(|group| *group != metadata.gid())
            .unwrap_or(metadata.gid())
    };
    let owner = if metadata.uid() == 0 {
        1
    } else {
        metadata.uid()
    };
    chown(&path, Some(owner), Some(group)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
    atomic_write(&path, b"after").unwrap();
    let saved = fs::metadata(&path).unwrap();
    assert_eq!(saved.uid(), owner);
    assert_eq!(saved.gid(), group);
    assert_eq!(saved.mode() & 0o7777, 0o660);
    assert_eq!(fs::read(&path).unwrap(), b"after");
}

#[test]
fn exact_empty_write_rejects_external_deletion_but_missing_write_creates() {
    let root = tempdir().unwrap();
    let path = root.path().join("empty.txt");
    fs::write(&path, b"").unwrap();
    let saved = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        checked_atomic_write(&path, b"edited", &WriteExpectation::Exact(saved)),
        Err(VfsError::ExternalModification(_))
    ));
    assert!(!path.exists());
    checked_atomic_write(&path, b"new", &WriteExpectation::Missing).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
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

// Unix paths can contain arbitrary non-UTF-8 bytes.
#[cfg(unix)]
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
    assert_eq!(
        discovery
            .records
            .iter()
            .find(|(path, _)| path.ends_with("legacy.toml"))
            .unwrap()
            .1
            .path(),
        Some("old.txt".into())
    );
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
    assert!(crate::files::recovery_size_allowed(
        RECOVERY_PAYLOAD_BYTE_LIMIT - 8192
    ));
    assert!(!crate::files::recovery_size_allowed(
        RECOVERY_PAYLOAD_BYTE_LIMIT - 8191
    ));
}

#[cfg(windows)]
#[test]
fn recovery_paths_round_trip_windows_wide_names() {
    use std::os::windows::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_wide(&[
        b'C' as u16,
        b':' as u16,
        b'\\' as u16,
        0xd800,
    ]));
    let record = RecoveryRecord::new(Some(&path), 1, "draft".into());
    assert_eq!(record.path(), Some(path));
    let encoded = toml::to_string(&record).unwrap();
    assert_eq!(toml::from_str::<RecoveryRecord>(&encoded).unwrap(), record);
}

#[test]
fn recovery_safety_limits_are_testable_without_large_disk_fixtures() {
    assert!(recovery_journal_size_allowed(RECOVERY_JOURNAL_BYTE_LIMIT));
    assert!(!recovery_journal_size_allowed(
        RECOVERY_JOURNAL_BYTE_LIMIT + 1
    ));
    assert!(recovery_payload_size_allowed(
        RECOVERY_PAYLOAD_BYTE_LIMIT,
        0
    ));
    assert!(!recovery_payload_size_allowed(
        RECOVERY_PAYLOAD_BYTE_LIMIT,
        1
    ));
    let (paths, truncated) = bounded_recovery_entries(
        (0..=RECOVERY_DIRECTORY_ENTRY_LIMIT)
            .map(|index| std::path::PathBuf::from(format!("non-journal-{index}"))),
    );
    assert!(truncated);
    assert_eq!(paths.len(), RECOVERY_DIRECTORY_ENTRY_LIMIT);
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
    let root = tempdir().expect("tempdir");
    let path = root.path().join("invalid.toml");
    fs::write(&path, "1234").unwrap();
    let exact = crate::files::vfs::read_file_bounded(&path, 4).expect("exact limit");
    assert_eq!(exact, b"1234");
    fs::write(&path, "12345").unwrap();
    assert!(matches!(
        crate::files::vfs::read_file_bounded(&path, 4),
        Err(VfsError::DocumentTooLarge {
            maximum_bytes: 4,
            ..
        })
    ));
    fs::write(&path, [0xff]).expect("invalid journal");
    let invalid = crate::files::recovery::read_recovery_source(&path).expect_err("invalid UTF-8");
    assert!(invalid.starts_with("recovery journal is not UTF-8:"));
    let discovery = discover_recovery(root.path());
    assert!(discovery.records.is_empty());
    assert!(discovery.diagnostics[0].contains("recovery journal is not UTF-8"));
    let cleanup = cleanup_recovery(root.path(), 0, 0, crate::clock::system_time(), &[]);
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
    let limited = crate::files::recovery::discover_recovery_bounded(root.path(), 1, 100);
    assert_eq!(limited.records.len(), 1);
    let limited = crate::files::recovery::discover_recovery_bounded(root.path(), 100, 8);
    assert_eq!(limited.records.len(), 2);
    assert_eq!(limited.diagnostics.len(), 1);
    assert!(limited.diagnostics[0].contains("byte safety limit"));
    assert!(
        crate::files::recovery::discover_recovery_bounded(root.path(), 0, 100)
            .records
            .is_empty()
    );
    let cleanup = cleanup_recovery(root.path(), 0, 0, crate::clock::system_time(), &[]);
    assert_eq!(cleanup.deleted.len(), 3);
}

#[test]
fn recovery_discovery_keeps_the_same_newest_journals_as_cleanup() {
    let root = tempdir().unwrap();
    let now = crate::clock::system_time();
    let mut newest = Vec::new();
    for index in 0..200 {
        let path = root.path().join(format!("{index:03}.toml"));
        write_recovery(&path, &RecoveryRecord::new(None, index, "unsaved".into())).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(now - Duration::from_secs(200 - index))
            .unwrap();
        if index >= 100 {
            newest.push(path);
        }
    }
    newest.reverse();
    let discovery = discover_recovery(root.path());
    assert!(discovery.diagnostics.is_empty());
    assert_eq!(
        discovery
            .records
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>(),
        newest
    );
    let cleanup = cleanup_recovery(root.path(), 100, 0, now, &[]);
    assert_eq!(cleanup.deleted.len(), 100);
    assert!(cleanup.diagnostics.is_empty());
    assert!(discovery.records.iter().all(|(path, _)| path.exists()));
    assert_eq!(discover_recovery(root.path()).records, discovery.records);
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

    let cleanup = cleanup_recovery(root.path(), 1, 0, crate::clock::system_time(), &[]);
    assert_eq!(cleanup.deleted.len(), 1);
    assert!(
        corrupt.exists(),
        "corrupt journals require explicit inspection"
    );
    assert_eq!(discover_recovery(root.path()).records.len(), 1);

    let future = crate::clock::system_time() + Duration::from_secs(2 * 86_400);
    let cleanup = cleanup_recovery(root.path(), 10, 1, future, &[]);
    assert_eq!(cleanup.deleted.len(), 1, "age policy removes the survivor");
    assert!(corrupt.exists());
}

#[test]
fn recovery_replacement_waits_for_the_directory_lock() {
    let root = tempdir().unwrap();
    let journal = root.path().join("buffer.toml");
    let lock = crate::files::vfs::lock_file(&root.path().join(".recovery.lock")).unwrap();
    let (started, starting) = std::sync::mpsc::channel();
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        finished
            .send(write_recovery(
                &journal,
                &RecoveryRecord::new(None, 2, "fresh".into()),
            ))
            .unwrap();
    });
    starting.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(!root.path().join("buffer.toml").exists());
    drop(lock);
    completion
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert_eq!(discover_recovery(root.path()).records[0].1.content, "fresh");
}

#[test]
fn recovery_cleanup_waits_for_replacement_before_deciding_retention() {
    let root = tempdir().unwrap();
    let journal = root.path().join("buffer.toml");
    write_recovery(&journal, &RecoveryRecord::new(None, 1, "old".into())).unwrap();
    let now = crate::clock::system_time();
    fs::File::options()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_modified(now - Duration::from_secs(2 * 86_400))
        .unwrap();
    let lock = crate::files::vfs::lock_file(&root.path().join(".recovery.lock")).unwrap();
    let directory = root.path().to_path_buf();
    let (started, starting) = std::sync::mpsc::channel();
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        finished
            .send(cleanup_recovery(&directory, 10, 1, now, &[]))
            .unwrap();
    });
    starting.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    // Finish the replacement while owning the directory lock, as write_recovery does.
    let fresh = RecoveryRecord::new(None, 2, "fresh".into());
    atomic_write(&journal, toml::to_string(&fresh).unwrap().as_bytes()).unwrap();
    drop(lock);
    let cleanup = completion.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(cleanup.deleted.is_empty());
    assert!(cleanup.diagnostics.is_empty());
    assert_eq!(
        discover_recovery(root.path()).records,
        vec![(journal, fresh)]
    );
}

#[test]
fn recovery_cleanup_preserves_protected_journals_beyond_count_and_age_limits() {
    let root = tempdir().unwrap();
    let live = root.path().join("live.toml");
    let old = root.path().join("old.toml");
    for path in [&live, &old] {
        write_recovery(path, &RecoveryRecord::new(None, 1, "draft".into())).unwrap();
    }
    let future = crate::clock::system_time() + Duration::from_secs(31 * 86_400);
    let cleanup = cleanup_recovery(root.path(), 0, 30, future, std::slice::from_ref(&live));
    assert_eq!(cleanup.deleted, vec![old]);
    assert!(live.exists());
}

#[test]
fn recovery_worker_fifo_order_makes_delete_the_revision_barrier() {
    let root = tempdir().expect("tempdir");
    let journal = root.path().join("buffer.toml");
    let mut worker = VfsWorker::start(4);
    fn expect_completion(
        worker: &mut VfsWorker,
        id: crate::files::RequestId,
        path: &Path,
        write: bool,
    ) {
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
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    service: &WorkspaceSearchWorker,
) -> (Vec<WorkspaceSearchMatch>, WorkspaceSearchSummary) {
    // Debug builds scan megabyte fixtures slowly on a busy machine.
    let deadline = Instant::now() + Duration::from_secs(30);
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

fn collect_files(
    policy: crate::files::WorkspaceFilePolicy,
) -> (
    Vec<crate::files::WorkspaceFileEntry>,
    crate::files::WorkspaceFileSummary,
) {
    let mut service = crate::files::WorkspaceFileWorker::default();
    let stream_id = service.start(policy.clone()).expect("start finder");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut entries = Vec::new();
    loop {
        match service.try_event() {
            Some(crate::files::WorkspaceFileEvent::Batch {
                stream_id: actual,
                root_revision,
                entries: batch,
            }) => {
                assert_eq!((actual, root_revision), (stream_id, policy.root_revision));
                entries.extend(batch);
            }
            Some(crate::files::WorkspaceFileEvent::Complete {
                stream_id: actual,
                root_revision,
                summary,
            }) => {
                assert_eq!((actual, root_revision), (stream_id, policy.root_revision));
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

// Creates Unix FIFOs and symlinks.
#[cfg(unix)]
#[test]
fn linked_worktree_metadata_rejects_nonregular_and_oversized_files() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let git = root.path().join(".git");
    fs::write(&git, "x".repeat(4097)).unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    fs::remove_file(&git).unwrap();
    symlink("/dev/zero", &git).unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    fs::remove_file(&git).unwrap();
    nix::unistd::mkfifo(
        &git,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    let mut state = crate::editor::EditorState::new(root.path().into());
    assert_eq!(state.workspace_repository, None);
    state.set_workspace_root(root.path().join("other"));
    state.set_workspace_root(root.path().into());
    assert_eq!(state.workspace_repository, None);
}

#[test]
fn a_root_shows_its_worktrees_and_a_worktree_shows_only_itself() {
    if !isolated_git_test() {
        return;
    }
    let repository = repository_with_worktree();
    let root = crate::files::canonicalize(repository.path()).expect("root");
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
        let mut service = WorkspaceSearchWorker::default();
        service.start(search_policy(root, "needle")).expect("start");
        let mut paths = collect_search(&service)
            .0
            .into_iter()
            .map(|found| found.relative_path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    };
    // Grep and the finder leave out what Git ignores, the worktrees among it.
    assert_eq!(searched(&root), ["main.txt"]);
    assert_eq!(searched(&worktree), ["feat.txt"]);

    let found = |root: &Path| {
        let mut service = crate::files::WorkspaceFileWorker::default();
        service
            .start(crate::files::WorkspaceFilePolicy {
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
                Some(crate::files::WorkspaceFileEvent::Batch { entries: batch, .. }) => {
                    entries.extend(batch)
                }
                Some(crate::files::WorkspaceFileEvent::Complete { .. }) => break,
                Some(crate::files::WorkspaceFileEvent::Failed { error, .. }) => panic!("{error}"),
                _ => {}
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        entries
            .iter()
            .map(|entry: &crate::files::WorkspaceFileEntry| {
                entry.relative_path.to_string_lossy().into_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(found(&root), ["main.txt"]);
    assert_eq!(found(&worktree), ["feat.txt"]);

    assert_eq!(
        crate::files::linked_worktree_repository(&worktree).as_deref(),
        root.file_name().and_then(|name| name.to_str())
    );
    assert_eq!(crate::files::linked_worktree_repository(&root), None);
    fs::write(worktree.join(".git"), "gitdir: ../../.git/worktrees/feat\n").unwrap();
    assert_eq!(
        crate::files::linked_worktree_repository(&worktree).as_deref(),
        root.file_name().and_then(|name| name.to_str())
    );
}

// Creates Unix FIFOs and symlinks.
#[cfg(unix)]
#[test]
fn linked_worktree_metadata_accepts_valid_paths_and_rejects_nonregular_and_oversized_files() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    let root = tempdir().unwrap();
    let git = root.path().join(".git");
    mkfifo(&git, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    fs::remove_file(&git).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &git).unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    fs::remove_file(&git).unwrap();
    fs::write(
        &git,
        format!("gitdir: /repo/.git/worktrees/{}", "x".repeat(16 * 1024)),
    )
    .unwrap();
    assert_eq!(crate::files::linked_worktree_repository(root.path()), None);
    fs::write(&git, "gitdir: /repo/.git/worktrees/feature\n").unwrap();
    assert_eq!(
        crate::files::linked_worktree_repository(root.path()).as_deref(),
        Some("repo")
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
    assert!(
        crate::files::directory::root_worktrees(root.path(), || {
            scanned += 1;
            true
        })
        .next()
        .is_some()
    );
    assert_eq!(
        scanned, 1,
        "asking for one worktree inspects only one candidate"
    );
    assert_eq!(
        crate::files::directory::root_worktrees(root.path(), || false).count(),
        0
    );
    let mut remaining = 2;
    let paths = crate::files::directory::root_worktrees(root.path(), || {
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
        true
    })
    .collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
}

#[test]
fn worktree_discovery_respects_the_listing_limit() {
    if !isolated_git_test() {
        return;
    }
    let repository = repository_with_worktree();
    let root = crate::files::canonicalize(repository.path()).unwrap();
    let listing =
        list_scoped_directory(1, 0, &root, std::slice::from_ref(&root), false, false, 1).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.truncated, "the extra worktree entry did not fit");
}

#[cfg(unix)]
#[test]
fn worktree_symlinks_cannot_add_outside_files_to_finder_or_grep() {
    if !isolated_git_test() {
        return;
    }
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let outside = repository_with_worktree();
    let worktrees = root.path().join(".worktree");
    symlink(outside.path().join(".worktree"), &worktrees).unwrap();
    for linked_directory in [true, false] {
        if !linked_directory {
            fs::remove_file(&worktrees).unwrap();
            fs::create_dir(&worktrees).unwrap();
            symlink(
                outside.path().join(".worktree/feat"),
                worktrees.join("feat"),
            )
            .unwrap();
        }
        assert_eq!(
            crate::files::directory::root_worktrees(root.path(), || true).count(),
            0
        );
        let (entries, summary) = collect_files(crate::files::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf()],
            root_revision: 1,
            include_hidden: false,
            include_ignored: false,
            maximum_files: 10,
            maximum_path_bytes: 4096,
            batch_size: 1,
            event_queue_capacity: 1,
        });
        assert!(entries.is_empty());
        assert!(!summary.truncated);
        let mut search = WorkspaceSearchWorker::default();
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
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().expect("root");
    fs::create_dir(root.path().join("z-dir")).expect("dir");
    fs::create_dir(root.path().join("dir10")).expect("dir10");
    fs::create_dir(root.path().join("dir2")).expect("dir2");
    fs::write(root.path().join("a-file"), "a").expect("file");
    fs::write(root.path().join("file10"), "10").expect("file10");
    fs::write(root.path().join("file2"), "2").expect("file2");
    fs::write(root.path().join(".hidden"), "h").expect("hidden");
    let canonical = crate::files::canonicalize(root.path()).expect("canonical");
    let mut service = DirectoryWorker::default();
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
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let root = tempdir().expect("root");
    let outside = tempdir().expect("outside");
    symlink(outside.path(), root.path().join("escape")).expect("symlink");
    let invalid = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
    let invalid_created = fs::write(root.path().join(invalid), "x").is_ok();
    let canonical = crate::files::canonicalize(root.path()).expect("canonical");
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

// Unix paths can contain arbitrary non-UTF-8 bytes.
#[cfg(unix)]
#[test]
fn workspace_files_and_search_keep_distinct_non_utf8_path_identities() {
    if !isolated_git_test() {
        return;
    }
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
        crate::files::workspace_files::file_id(&paths[0]),
        crate::files::workspace_files::file_id(&paths[1])
    );
    for path in paths {
        if let Err(error) = fs::write(&path, "needle") {
            // Some filesystems require UTF-8 names; the identity checks above still apply.
            assert_eq!(error.raw_os_error(), Some(nix::libc::EILSEQ));
            return;
        }
    }
    let mut service = crate::files::WorkspaceFileWorker::default();
    service
        .start(crate::files::WorkspaceFilePolicy {
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
            Some(crate::files::WorkspaceFileEvent::Batch { entries: batch, .. }) => {
                entries.extend(batch)
            }
            Some(crate::files::WorkspaceFileEvent::Complete { .. }) => break,
            Some(crate::files::WorkspaceFileEvent::Failed { error, .. }) => panic!("{error}"),
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
    let mut search = WorkspaceSearchWorker::default();
    search.start(search_policy(root.path(), "needle")).unwrap();
    let (matches, _) = collect_search(&search);
    assert_eq!(matches.len(), 2);
    assert_ne!(matches[0].id, matches[1].id);
}

#[test]
fn workspace_event_senders_wake_when_cancel_drops_a_full_queue() {
    use crate::files::workspace_files::send_file_event;
    use crate::files::workspace_search::send_search_event;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(WorkspaceSearchEvent::Cancelled { stream_id: 1 })
        .unwrap();
    let worker_cancel = Arc::clone(&cancel);
    let (finished, completion) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let sent = send_search_event(
            &sender,
            &worker_cancel,
            WorkspaceSearchEvent::Cancelled { stream_id: 2 },
        );
        finished.send(sent).unwrap();
    });
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    cancel.store(true, Ordering::Release);
    drop(receiver);
    assert!(!completion.recv_timeout(Duration::from_secs(2)).unwrap());
    worker.join().unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(crate::files::WorkspaceFileEvent::Cancelled {
            stream_id: 1,
            root_revision: 1,
        })
        .unwrap();
    let worker_cancel = Arc::clone(&cancel);
    let (finished, completion) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let sent = send_file_event(
            &sender,
            &worker_cancel,
            crate::files::WorkspaceFileEvent::Cancelled {
                stream_id: 2,
                root_revision: 1,
            },
        );
        finished.send(sent).unwrap();
    });
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    cancel.store(true, Ordering::Release);
    drop(receiver);
    assert!(!completion.recv_timeout(Duration::from_secs(2)).unwrap());
    worker.join().unwrap();
}

#[test]
fn workspace_search_streams_batches_before_reading_later_files_and_reports_read_failure() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    let root = tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "needle needle").unwrap();
    let later = root.path().join("b.txt");
    fs::write(&later, "needle").unwrap();
    let mut policy = search_policy(root.path(), "needle");
    policy.batch_size = 1;
    // Each batch must be received before the worker proceeds to the next one.
    let (sender, receiver) = mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        crate::files::workspace_search::run_workspace_search(
            1,
            policy,
            Arc::new(AtomicBool::new(false)),
            sender,
        );
    });
    let WorkspaceSearchEvent::Batch { matches, .. } =
        receiver.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected the first file's first match");
    };
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].relative_path, Path::new("a.txt"));
    fs::remove_file(later).unwrap();
    let WorkspaceSearchEvent::Batch { matches, .. } =
        receiver.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected the first file's second match");
    };
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].relative_path, Path::new("a.txt"));
    let WorkspaceSearchEvent::Complete { summary, .. } =
        receiver.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected completion after the unreadable file");
    };
    assert_eq!(summary.files_searched, 1);
    assert_eq!(summary.walk_errors, 1);
    assert!(
        summary.truncated,
        "an incomplete scan must not allow workspace replacement"
    );
    worker.join().unwrap();
}

#[test]
fn workspace_search_positions_many_matches_after_a_large_prefix() {
    if !isolated_git_test() {
        return;
    }
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
    let mut service = WorkspaceSearchWorker::default();
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
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().expect("root");
    fs::write(root.path().join("a.txt"), "café CAFE cat scatter cat\n").expect("a");
    fs::write(root.path().join("bom.txt"), b"\xef\xbb\xbfcat\r\n").expect("bom");
    let mut service = WorkspaceSearchWorker::default();
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
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().expect("root");
    fs::create_dir(root.path().join(".git")).expect("git");
    fs::write(root.path().join(".gitignore"), "ignored.txt\n").expect("ignore");
    fs::write(root.path().join("visible.rs"), "needle\n").expect("visible");
    fs::write(root.path().join("ignored.txt"), "needle\n").expect("ignored");
    fs::write(root.path().join(".hidden.rs"), "needle\n").expect("hidden");
    fs::write(root.path().join("binary.rs"), b"needle\0binary").expect("binary");
    fs::write(root.path().join("invalid.rs"), b"needle\xff").expect("invalid UTF-8");
    fs::write(root.path().join("other.txt"), "needle\n").expect("other");
    let mut service = WorkspaceSearchWorker::default();
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
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().expect("root");
    fs::write(
        root.path().join("long"),
        format!("needle{}", "x".repeat(10_000)),
    )
    .expect("long line");
    let mut service = WorkspaceSearchWorker::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.maximum_preview_bytes = 16;
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert!(matches[0].line_text.len() <= 16);
}

#[test]
fn workspace_search_obeys_per_file_and_aggregate_byte_limits() {
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().expect("root");
    fs::write(root.path().join("a"), "needle plus").expect("a");
    fs::write(root.path().join("b"), "needle plus").expect("b");
    let mut service = WorkspaceSearchWorker::default();
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
    if !isolated_git_test() {
        return;
    }
    use std::os::unix::fs::symlink;
    let root = tempdir().expect("root");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested");
    fs::write(nested.join("inside"), "needle").expect("inside");
    let outside = tempdir().expect("outside");
    fs::write(outside.path().join("secret"), "needle").expect("secret");
    symlink(outside.path(), root.path().join("escape")).expect("link");
    let mut service = WorkspaceSearchWorker::default();
    let mut policy = search_policy(root.path(), "needle");
    policy.roots.push(nested.clone());
    service.start(policy).expect("start");
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].root, nested);
}

#[test]
fn workspace_search_overlapping_roots_do_not_exhaust_the_unique_file_limit() {
    let root = tempdir().unwrap();
    let first = root.path().join("a");
    let nested = first.join("nested");
    let last = root.path().join("z");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(&last).unwrap();
    for path in [first.join("one"), nested.join("two"), last.join("three")] {
        fs::write(path, "needle").unwrap();
    }
    let mut policy = search_policy(&first, "needle");
    policy.roots.extend([nested, last]);
    policy.maximum_files = 3;
    let mut service = WorkspaceSearchWorker::default();
    service.start(policy).unwrap();
    let (matches, summary) = collect_search(&service);
    assert_eq!(matches.len(), 3);
    assert_eq!(summary.files_considered, 3);
    assert!(!summary.truncated);
}

#[cfg(unix)]
#[test]
fn workspace_search_rejects_worktrees_behind_an_escaping_directory_symlink() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    fs::write(root.path().join("inside"), "needle").unwrap();
    let outside = tempdir().unwrap();
    let checkout = outside.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join(".git"), "gitdir: /elsewhere").unwrap();
    fs::write(checkout.join("secret"), "needle").unwrap();
    symlink(outside.path(), root.path().join(".worktree")).unwrap();
    let mut service = WorkspaceSearchWorker::default();
    service.start(search_policy(root.path(), "needle")).unwrap();
    let (matches, _) = collect_search(&service);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].relative_path, Path::new("inside"));
}

#[test]
fn workspace_search_replacement_cancels_old_stream_id_and_caps_results() {
    let root = tempdir().expect("root");
    for index in 0..100 {
        fs::write(root.path().join(format!("{index:03}.txt")), "needle needle").expect("file");
    }
    let mut service = WorkspaceSearchWorker::default();
    let first = service
        .start(search_policy(root.path(), "never"))
        .expect("first");
    let mut policy = search_policy(root.path(), "needle");
    policy.maximum_results = 3;
    policy.batch_size = 1;
    let second = service.start(policy).expect("second");
    assert!(second > first);
    let (matches, summary) = collect_search(&service);
    assert_eq!(matches.len(), 3);
    assert!(summary.truncated);
    assert!(matches.windows(2).all(|pair| pair[0].id < pair[1].id));
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
            vec![crate::files::canonicalize(root.path()).expect("canonical root")],
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
fn directory_completion_filters_before_limiting_entries() {
    let root = tempdir().unwrap();
    for index in 0..1100 {
        fs::write(root.path().join(format!("entry-{index:04}.txt")), b"").unwrap();
    }
    let target = fs::read_dir(root.path())
        .unwrap()
        .nth(1050)
        .unwrap()
        .unwrap()
        .path();
    let prefix = target.file_name().unwrap().to_string_lossy().to_lowercase();
    let mut worker = VfsWorker::start(2);
    let id = worker
        .list_directory(root.path().to_path_buf(), prefix, 1000)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Directory {
            id: actual, result, ..
        }) = worker.try_result().unwrap()
        {
            assert_eq!(id, actual);
            assert_eq!(result.unwrap(), vec![target]);
            break;
        }
        assert!(Instant::now() < deadline, "directory completion timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn directory_completion_filters_names_before_capping_worker_results() {
    let root = tempdir().expect("tempdir");
    let names = (0..1005)
        .map(|index| format!("File-{index:04}.txt"))
        .collect::<Vec<_>>();
    for name in &names {
        fs::write(root.path().join(name), b"").expect("file");
    }
    let initial =
        crate::files::vfs::list_directory(root.path(), "", 1000).expect("unfiltered listing");
    assert_eq!(initial.len(), 1000);
    let target = names
        .iter()
        .find(|name| !initial.contains(&root.path().join(name)))
        .unwrap();
    let mut worker = VfsWorker::start(2);
    let id = worker
        .list_directory(root.path().to_path_buf(), target.to_uppercase(), 1000)
        .expect("queue filtered list");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(VfsResult::Directory {
            id: result_id,
            result,
            ..
        }) = worker.try_result().expect("worker")
        {
            assert_eq!(result_id, id);
            assert_eq!(
                result.expect("filtered listing"),
                vec![root.path().join(target)]
            );
            break;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// Checks Unix permission bits.
#[cfg(unix)]
#[test]
fn explorer_copies_preserve_private_and_executable_permissions() {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let roots = vec![crate::files::canonicalize(root.path()).unwrap()];
    for mode in [0o600, 0o751] {
        let source = root.path().join(format!("source-{mode:o}"));
        let target = root.path().join(format!("target-{mode:o}"));
        fs::write(&source, b"private content").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
        crate::files::vfs::apply_explorer_mutation(
            &ExplorerMutation::Copy {
                source,
                target: target.clone(),
            },
            &roots,
            1024,
        )
        .unwrap();
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, mode);
        assert_eq!(fs::read(&target).unwrap(), b"private content");
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
                vec![crate::files::canonicalize(root).expect("canonical root")],
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
fn workspace_file_delivery_waits_for_capacity_and_can_be_cancelled() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let event = crate::files::WorkspaceFileEvent::Cancelled {
        stream_id: 1,
        root_revision: 2,
    };
    sender.send(event.clone()).unwrap();
    let worker_cancel = Arc::clone(&cancel);
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done.send(crate::files::workspace_files::send_file_event(
            &sender,
            &worker_cancel,
            event,
        ))
        .unwrap();
    });
    assert!(matches!(
        completed.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    cancel.store(true, Ordering::Release);
    assert!(!completed.recv_timeout(Duration::from_secs(2)).unwrap());
    worker.join().unwrap();
    assert!(receiver.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
}

// Creates Unix symlinks.
#[cfg(unix)]
#[test]
fn workspace_files_reject_worktrees_behind_a_symlinked_ancestor() {
    let root = repository_with_worktree();
    let outside = repository_with_worktree();
    fs::remove_dir_all(root.path().join(".worktree")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join(".worktree"),
        root.path().join(".worktree"),
    )
    .unwrap();
    let mut service = crate::files::WorkspaceFileWorker::default();
    service
        .start(crate::files::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf()],
            root_revision: 1,
            include_hidden: false,
            include_ignored: false,
            maximum_files: 100,
            maximum_path_bytes: 4096,
            batch_size: 10,
            event_queue_capacity: 8,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut paths = Vec::new();
    loop {
        match service.try_event() {
            Some(crate::files::WorkspaceFileEvent::Batch { entries, .. }) => {
                paths.extend(entries.into_iter().map(|entry| entry.relative_path));
            }
            Some(crate::files::WorkspaceFileEvent::Complete { summary, .. }) => {
                assert_eq!(summary.files, 1);
                break;
            }
            Some(crate::files::WorkspaceFileEvent::Failed { error, .. }) => panic!("{error}"),
            _ => {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(paths, vec![std::path::PathBuf::from("main.txt")]);
}

#[test]
fn workspace_files_report_a_missing_root_instead_of_partial_success() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("present.txt"), b"present").unwrap();
    let missing = root.path().join("missing-root");
    let mut service = crate::files::WorkspaceFileWorker::default();
    service
        .start(crate::files::WorkspaceFilePolicy {
            roots: vec![root.path().to_path_buf(), missing.clone()],
            root_revision: 7,
            include_hidden: false,
            include_ignored: false,
            maximum_files: 100,
            maximum_path_bytes: 4096,
            batch_size: 10,
            event_queue_capacity: 8,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match service.try_event() {
            Some(crate::files::WorkspaceFileEvent::Failed {
                root_revision,
                error,
                ..
            }) => {
                assert_eq!(root_revision, 7);
                assert!(error.contains("incomplete"), "{error}");
                assert!(error.contains(&missing.display().to_string()), "{error}");
                break;
            }
            Some(event) => panic!("unexpected successful discovery event: {event:?}"),
            None => {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn workspace_files_are_bounded_deterministic_and_ignore_symlink_escapes() {
    if !isolated_git_test() {
        return;
    }
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
    let mut service = crate::files::WorkspaceFileWorker::default();
    let stream_id = service
        .start(crate::files::WorkspaceFilePolicy {
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
                crate::files::WorkspaceFileEvent::Batch {
                    stream_id: g,
                    root_revision,
                    entries: batch,
                } => {
                    assert_eq!((g, root_revision), (stream_id, 7));
                    entries.extend(batch)
                }
                crate::files::WorkspaceFileEvent::Complete { summary, .. } => break summary,
                crate::files::WorkspaceFileEvent::Failed { error, .. } => panic!("{error}"),
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
            .map(|e| e.relative_path.clone())
            .collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("b.txt"),
            Path::new("sub").join("a.txt")
        ]
    );
}

#[test]
fn workspace_file_count_and_path_byte_limits_report_truncation() {
    if !isolated_git_test() {
        return;
    }
    let root = tempdir().unwrap();
    for name in ["a", "b", "c"] {
        fs::write(root.path().join(name), "text").unwrap();
    }
    let mut policy = crate::files::WorkspaceFilePolicy {
        roots: vec![root.path().to_path_buf()],
        root_revision: 7,
        include_hidden: false,
        include_ignored: false,
        maximum_files: 1,
        maximum_path_bytes: 4096,
        batch_size: 1,
        event_queue_capacity: 1,
    };
    let (entries, summary) = collect_files(policy.clone());
    assert_eq!(entries.len(), 1);
    assert_eq!(summary.files, 1);
    assert!(summary.truncated);
    let cost =
        entries[0].canonical_path.as_os_str().len() + entries[0].relative_path.as_os_str().len();
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
    let result = crate::files::project_root::resolve_project_root(
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
        Some(crate::files::canonicalize(root.path()).unwrap())
    );
    std::fs::remove_file(root.path().join("example.opam")).unwrap();
    let result = crate::files::project_root::resolve_project_root(
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
