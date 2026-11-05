use std::{fs, path::Path};

use crate::editor::text::{BufferRevision, ByteOffset, ByteRange, TextEdit};

use super::trust::{TRUST_STATE_BYTE_LIMIT, load_trust_database, persist_trust_database};
use super::*;
use tempfile::tempdir;

fn workspace(root: &Path) -> WorkspaceState {
    WorkspaceState::new(
        WorkspaceId(1),
        [(
            "root".into(),
            RootIdentity::capture(root).expect("identity"),
        )],
    )
    .expect("workspace")
}

#[test]
fn multi_root_prefers_the_narrowest_containing_root_and_defaults_untrusted() {
    let directory = tempdir().expect("tempdir");
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).expect("nested");
    let workspace = WorkspaceState::new(
        WorkspaceId(7),
        [
            (
                "outer".into(),
                RootIdentity::capture(directory.path()).expect("outer"),
            ),
            (
                "inner".into(),
                RootIdentity::capture(&nested).expect("inner"),
            ),
        ],
    )
    .expect("workspace");
    let outer = &workspace.roots()[0].identity.canonical_path;
    let nested = &workspace.roots()[1].identity.canonical_path;
    assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
    assert_eq!(
        workspace
            .root_for_canonical_path(&nested.join("file"))
            .map(|root| root.name.as_str()),
        Some("inner")
    );
    assert!(
        workspace
            .require_path(Path::new("/definitely/outside"))
            .is_err()
    );
    assert!(
        workspace
            .require_path(&outer.join("nested/../../escape"))
            .is_err()
    );
}

#[test]
fn root_ids_are_monotonic_removal_has_fallback_and_root_changes_revoke_trust() {
    let parent = tempdir().expect("parent");
    let first = parent.path().join("first");
    let second = parent.path().join("second");
    let third = parent.path().join("third");
    fs::create_dir(&first).expect("first");
    fs::create_dir(&second).expect("second");
    fs::create_dir(&third).expect("third");
    let mut workspace = workspace(&first);
    workspace.set_trust(WorkspaceTrust::Trusted);
    let second_id = workspace
        .add_root(
            "second".into(),
            RootIdentity::capture(&second).expect("identity"),
        )
        .expect("add second");
    assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
    workspace.set_trust(WorkspaceTrust::Trusted);
    let removed = workspace.remove_root(second_id).expect("remove active");
    assert_eq!(removed.id, second_id);
    assert_eq!(
        workspace.active_root(),
        workspace.roots().first().map(|root| root.id)
    );
    assert_eq!(workspace.trust(), WorkspaceTrust::Untrusted);
    let third_id = workspace
        .add_root(
            "third".into(),
            RootIdentity::capture(&third).expect("identity"),
        )
        .expect("add third");
    assert!(third_id.0 > second_id.0, "root IDs must not be recycled");
    assert!(matches!(
        workspace.add_root(
            "duplicate".into(),
            RootIdentity {
                canonical_path: fs::canonicalize(&third).expect("canonical"),
                filesystem_identity: Some((u64::MAX, u64::MAX)),
            }
        ),
        Err(WorkspaceError::DuplicateRoot(_))
    ));
    workspace.remove_root(third_id).expect("remove third");
    assert!(matches!(
        workspace.remove_root(workspace.roots()[0].id),
        Err(WorkspaceError::LastRoot)
    ));
}

#[test]
fn workspace_enforces_absolute_root_ceiling() {
    let parent = tempdir().expect("parent");
    let first = parent.path().join("root-0");
    fs::create_dir(&first).expect("first");
    let mut workspace = workspace(&first);
    for index in 1..ABSOLUTE_ROOT_LIMIT {
        let path = parent.path().join(format!("root-{index}"));
        fs::create_dir(&path).expect("root");
        workspace
            .add_root(
                format!("root-{index}"),
                RootIdentity::capture(&path).expect("identity"),
            )
            .expect("within limit");
    }
    let overflow = parent.path().join("overflow");
    fs::create_dir(&overflow).expect("overflow");
    assert!(matches!(
        workspace.add_root(
            "overflow".into(),
            RootIdentity::capture(&overflow).expect("identity")
        ),
        Err(WorkspaceError::TooManyRoots(ABSOLUTE_ROOT_LIMIT))
    ));
}

#[cfg(unix)]
#[test]
fn trust_identity_does_not_follow_a_retargeted_symlink() {
    use std::os::unix::fs::symlink;
    let directory = tempdir().expect("tempdir");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).expect("first");
    fs::create_dir(&second).expect("second");
    let link = directory.path().join("root");
    symlink(&first, &link).expect("link");
    let original = RootIdentity::capture(&link).expect("original");
    let mut store = TrustStore::default();
    store.trust(original.clone());
    fs::remove_file(&link).expect("unlink");
    symlink(&second, &link).expect("retarget");
    let changed = RootIdentity::capture(&link).expect("changed");
    assert!(store.is_trusted(&original));
    assert!(!store.is_trusted(&changed));
}

#[test]
fn trust_blocks_project_execution_but_not_system_tools() {
    let directory = tempdir().expect("tempdir");
    let mut workspace = workspace(directory.path());
    assert!(
        workspace
            .authorize_execution(ExecutionSource::SystemTool)
            .is_ok()
    );
    assert!(matches!(
        workspace.authorize_execution(ExecutionSource::ProjectLocal),
        Err(WorkspaceError::TrustRequired(_))
    ));
    workspace.set_trust(WorkspaceTrust::Trusted);
    assert!(
        workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
            .is_ok()
    );
}

fn diagnostic(uri: &str, version: u64, start: usize, message: &str) -> Diagnostic {
    Diagnostic {
        uri: uri.into(),
        version: Some(BufferRevision(version)),
        range: ByteRange::new(ByteOffset(start), ByteOffset(start + 1)).expect("range"),
        severity: DiagnosticSeverity::Warning,
        source: "fake".into(),
        code: None,
        message: message.into(),
        related: Vec::new(),
        tags: Vec::new(),
        fixes: Vec::new(),
        data: None,
        timestamp: crate::clock::system_time(),
    }
}

#[test]
fn trust_database_round_trips_exact_filesystem_identity_and_rejects_path_only_records() {
    let root = tempdir().expect("root");
    let state = tempdir().expect("state");
    let path = state.path().join("trust.toml");
    let identity = RootIdentity::capture(root.path()).expect("identity");
    let database = TrustDatabase {
        version: TRUST_STATE_VERSION,
        identities: vec![identity.clone()],
        seen: vec![identity.clone()],
    };
    persist_trust_database(&path, &database).expect("persist");
    assert_eq!(load_trust_database(&path).expect("load"), database);
    let changed = RootIdentity {
        canonical_path: identity.canonical_path.clone(),
        filesystem_identity: identity
            .filesystem_identity
            .map(|(device, inode)| (device, inode.saturating_add(1))),
    };
    let mut store = TrustStore::default();
    let loaded = load_trust_database(&path).expect("load");
    store.replace(loaded.identities);
    store.replace_seen(loaded.seen);
    assert!(store.is_seen(&identity));
    assert!(!store.is_trusted(&changed));
    store.revoke(&identity);
    assert!(!store.is_trusted(&identity));
    assert!(store.is_seen(&identity));
    assert!(matches!(
        persist_trust_database(
            &path,
            &TrustDatabase {
                version: TRUST_STATE_VERSION,
                identities: vec![RootIdentity {
                    canonical_path: identity.canonical_path,
                    filesystem_identity: None,
                }],
                seen: Vec::new(),
            }
        ),
        Err(TrustStateError::MissingFilesystemIdentity(_))
    ));
}

#[test]
fn legacy_trusted_workspaces_migrate_to_seen_without_reprompting() {
    let root = tempdir().expect("root");
    let state = tempdir().expect("state");
    let path = state.path().join("trust.toml");
    let identity = RootIdentity::capture(root.path()).expect("identity");
    let legacy = TrustDatabase {
        version: 1,
        identities: vec![identity.clone()],
        seen: Vec::new(),
    };
    fs::write(&path, toml::to_string(&legacy).expect("legacy TOML")).expect("legacy state");

    let migrated = load_trust_database(&path).expect("migrated state");
    assert_eq!(migrated.version, TRUST_STATE_VERSION);
    assert_eq!(migrated.identities, vec![identity.clone()]);
    assert_eq!(migrated.seen, vec![identity]);
}

#[test]
fn trust_database_missing_corrupt_oversize_and_atomic_failure_are_safe() {
    let state = tempdir().expect("state");
    let missing = state.path().join("missing");
    assert!(
        load_trust_database(&missing)
            .expect("missing")
            .identities
            .is_empty()
    );
    fs::write(&missing, "not = [valid").expect("corrupt");
    assert!(matches!(
        load_trust_database(&missing),
        Err(TrustStateError::Invalid { .. })
    ));
    fs::write(&missing, vec![b'x'; TRUST_STATE_BYTE_LIMIT as usize + 1]).expect("large");
    assert!(matches!(
        load_trust_database(&missing),
        Err(TrustStateError::TooLarge(_))
    ));
    let parent_file = state.path().join("parent-file");
    fs::write(&parent_file, "occupied").expect("parent file");
    assert!(
        persist_trust_database(&parent_file.join("trust.toml"), &TrustDatabase::default()).is_err()
    );
}

#[cfg(unix)]
#[test]
fn failed_trust_database_replacement_preserves_existing_bytes() {
    let root = tempdir().expect("root");
    let state = tempdir().expect("state");
    let path = state.path().join("trust.toml");
    let original = TrustDatabase::default();
    persist_trust_database(&path, &original).expect("persist original");
    let bytes = fs::read(&path).expect("original bytes");
    let identity = RootIdentity::capture(root.path()).expect("identity");
    let update = TrustDatabase {
        version: TRUST_STATE_VERSION,
        identities: vec![identity.clone()],
        seen: vec![identity],
    };
    // A trailing separator rejects the rename onto a regular file, even as root.
    let mut destination = path.as_os_str().to_os_string();
    destination.push("/");
    assert!(matches!(
        persist_trust_database(Path::new(&destination), &update),
        Err(TrustStateError::Io {
            operation: "replace",
            ..
        })
    ));
    assert_eq!(fs::read(&path).expect("preserved bytes"), bytes);
    assert_eq!(
        load_trust_database(&path).expect("preserved database"),
        original
    );
    assert_eq!(
        fs::read_dir(state.path()).expect("state directory").count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn persisted_trust_does_not_follow_replaced_root_inode() {
    let parent = tempdir().expect("parent");
    let root = parent.path().join("root");
    fs::create_dir(&root).expect("root");
    let original = RootIdentity::capture(&root).expect("original");
    let displaced = parent.path().join("displaced");
    fs::rename(&root, &displaced).expect("displace");
    fs::create_dir(&root).expect("replacement");
    let replacement = RootIdentity::capture(&root).expect("replacement identity");
    assert_eq!(original.canonical_path, replacement.canonical_path);
    assert_ne!(
        original.filesystem_identity,
        replacement.filesystem_identity
    );
    let mut store = TrustStore::default();
    store.trust(original);
    assert!(!store.is_trusted(&replacement));
    assert!(store.mark_seen(replacement.clone()));
    assert!(store.identities().is_empty());
    assert_eq!(store.seen_identities(), vec![replacement]);
}

#[test]
fn trust_worker_reports_distinct_generations_and_async_identity() {
    let root = tempdir().expect("root");
    let state = tempdir().expect("state");
    let mut service = WorkspaceTrustService::start(4);
    let identity_generation = service
        .inspect_root(root.path().to_path_buf())
        .expect("inspect");
    let load_generation = service.load(state.path().join("missing")).expect("load");
    assert!(load_generation > identity_generation);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut identity_seen = false;
    let mut load_seen = false;
    while !identity_seen || !load_seen {
        if let Some(event) = service.try_event().expect("event") {
            match event {
                TrustEvent::RootInspected { generation, result } => {
                    assert_eq!(generation, identity_generation);
                    assert!(result.expect("identity").filesystem_identity.is_some());
                    identity_seen = true;
                }
                TrustEvent::Loaded { generation, result } => {
                    assert_eq!(generation, load_generation);
                    assert!(result.expect("database").identities.is_empty());
                    load_seen = true;
                }
                TrustEvent::Persisted { .. } => panic!("unexpected persist"),
            }
        }
        assert!(std::time::Instant::now() < deadline, "worker timeout");
        std::thread::yield_now();
    }
}

#[test]
fn explorer_state_is_lazy_stable_and_rejects_stale_loads() {
    let root = tempdir().expect("root");
    let workspace = workspace(root.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 3);
    let root_generation = explorer.projection_generation();
    let root_id = explorer.selected_node().expect("selected root").id;
    assert_eq!(explorer.visible_ids(), &[root_id]);
    assert!(explorer.begin_load(root_id, 9));
    let loading_generation = explorer.projection_generation();
    assert!(loading_generation > root_generation);
    assert!(!explorer.apply_load(root_id, 8, 3, Vec::new()));
    assert!(!explorer.apply_load(root_id, 9, 2, Vec::new()));
    assert_eq!(explorer.projection_generation(), loading_generation);
    let root_path = &workspace.roots()[0].identity.canonical_path;
    let child_path = root_path.join("child");
    assert!(explorer.apply_load(
        root_id,
        9,
        3,
        vec![ExplorerChild {
            path: child_path.clone(),
            name: "child".into(),
            kind: ExplorerKind::Directory
        }]
    ));
    assert_eq!(explorer.visible().len(), 2);
    assert!(explorer.projection_generation() > loading_generation);
    let child_id = explorer.visible_ids()[1];
    assert!(explorer.begin_load(root_id, 10));
    assert!(explorer.apply_load(
        root_id,
        10,
        3,
        vec![ExplorerChild {
            path: child_path,
            name: "child".into(),
            kind: ExplorerKind::Directory,
        }]
    ));
    assert_eq!(explorer.visible_ids()[1], child_id, "stable path ID");
    let loaded_generation = explorer.projection_generation();
    assert!(explorer.begin_load(child_id, 11));
    assert!(explorer.apply_load(
        child_id,
        11,
        3,
        vec![ExplorerChild {
            path: root_path.join("child/nested"),
            name: "nested".into(),
            kind: ExplorerKind::File,
        }]
    ));
    let nested_id = explorer.visible_ids()[2];
    explorer.select(child_id);
    let nested_generation = explorer.projection_generation();
    explorer.move_selection(-1);
    assert_eq!(explorer.selected_id(), Some(root_id));
    assert_eq!(explorer.projection_generation(), nested_generation);

    explorer.select(nested_id);
    explorer.collapse_or_parent();
    assert_eq!(explorer.selected_id(), Some(child_id));
    explorer.collapse_or_parent();
    let collapsed_generation = explorer.projection_generation();
    assert_eq!(explorer.selected_id(), Some(child_id));
    assert_eq!(explorer.visible_ids(), &[root_id, child_id]);
    explorer.collapse_or_parent();
    assert_eq!(explorer.selected_id(), Some(root_id));
    assert_eq!(explorer.projection_generation(), collapsed_generation);

    explorer.select(root_id);
    explorer.toggle_loaded(root_id);
    assert!(explorer.projection_generation() > loaded_generation);
    assert_eq!(explorer.visible_ids(), &[root_id]);
    explorer.toggle_loaded(root_id);
    explorer.reset_roots(workspace.roots(), 4);
    assert_eq!(explorer.selected_node().map(|node| node.id), Some(root_id));
}

#[test]
fn explorer_rejects_children_from_a_replaced_directory() {
    let root = tempdir().unwrap();
    let workspace = workspace(root.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 1);
    let root_id = explorer.selected_id().unwrap();
    let child_path = root.path().join("child");
    assert!(explorer.begin_load(root_id, 1));
    assert!(explorer.apply_load(
        root_id,
        1,
        1,
        vec![ExplorerChild {
            path: child_path.clone(),
            name: "child".into(),
            kind: ExplorerKind::Directory,
        }]
    ));
    let child_id = explorer.visible_ids()[1];
    assert!(explorer.begin_load(child_id, 2));
    assert!(explorer.apply_load(
        child_id,
        2,
        1,
        vec![
            ExplorerChild {
                path: child_path.clone(),
                name: "child".into(),
                kind: ExplorerKind::Directory
            },
            ExplorerChild {
                path: root.path().join("sibling"),
                name: "sibling".into(),
                kind: ExplorerKind::Directory
            },
            ExplorerChild {
                path: child_path.join("valid"),
                name: "valid".into(),
                kind: ExplorerKind::File
            },
        ]
    ));
    assert_eq!(explorer.visible_ids().len(), 3);
    assert_eq!(explorer.visible_ids()[..2], [root_id, child_id]);
    explorer.select(child_id);
    assert_eq!(explorer.selected_node().unwrap().path, child_path);
    assert_eq!(explorer.selected_node().unwrap().children.len(), 1);
}

#[test]
fn explorer_replaces_large_listings_and_prunes_removed_expansions() {
    let root = tempdir().unwrap();
    let workspace = workspace(root.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 1);
    let root_id = explorer.selected_id().unwrap();
    let listing = |prefix: &str| {
        (0..10_000)
            .map(|index| ExplorerChild {
                path: root.path().join(format!("{prefix}{index}")),
                name: format!("{prefix}{index}"),
                kind: ExplorerKind::Directory,
            })
            .collect()
    };
    assert!(explorer.begin_load(root_id, 1));
    assert!(explorer.apply_load(root_id, 1, 1, listing("old")));
    let old_children = explorer.node(root_id).unwrap().children.clone();
    let old_directory = old_children[0];
    assert!(explorer.begin_load(old_directory, 2));
    assert!(explorer.apply_load(
        old_directory,
        2,
        1,
        vec![ExplorerChild {
            path: root.path().join("old0/nested"),
            name: "nested".into(),
            kind: ExplorerKind::Directory,
        }]
    ));
    let nested = explorer.node(old_directory).unwrap().children[0];
    assert!(explorer.begin_load(nested, 3));
    assert!(explorer.apply_load(nested, 3, 1, vec![]));
    explorer.select(nested);
    assert!(explorer.is_expanded(old_directory));
    assert!(explorer.is_expanded(nested));

    assert!(explorer.begin_load(root_id, 4));
    assert!(explorer.apply_load(root_id, 4, 1, listing("new")));
    assert_eq!(explorer.selected_id(), Some(root_id));
    assert_eq!(explorer.node(root_id).unwrap().children.len(), 10_000);
    assert_eq!(explorer.visible_ids().len(), 10_001);
    for old in old_children.into_iter().chain([nested]) {
        assert!(explorer.node(old).is_none());
        assert!(!explorer.is_expanded(old));
    }
    assert!(
        explorer
            .id_for_path(&root.path().join("old0/nested"))
            .is_none()
    );
    assert!(explorer.id_for_path(&root.path().join("new9999")).is_some());
    assert!(explorer.is_expanded(root_id));
}

#[test]
fn explorer_child_expands_collapsed_directories_and_selects_only_visible_children() {
    let root = tempdir().unwrap();
    let workspace = workspace(root.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 1);
    let root_id = explorer.selected_id().unwrap();
    explorer.begin_load(root_id, 1);
    explorer.apply_load(
        root_id,
        1,
        1,
        vec![ExplorerChild {
            path: root.path().join("directory"),
            name: "directory".into(),
            kind: ExplorerKind::Directory,
        }],
    );
    let directory = explorer.node(root_id).unwrap().children[0];
    explorer.begin_load(directory, 2);
    explorer.apply_load(
        directory,
        2,
        1,
        ["hidden", "match"]
            .into_iter()
            .map(|name| ExplorerChild {
                path: root.path().join("directory").join(name),
                name: name.into(),
                kind: ExplorerKind::File,
            })
            .collect(),
    );
    let children = explorer.node(directory).unwrap().children.clone();
    explorer.select(directory);
    explorer.collapse_or_parent();
    assert!(!explorer.is_expanded(directory));
    assert!(explorer.visible_index(children[0]).is_none());

    explorer.select_first_child();
    explorer.ensure_selection();
    assert!(explorer.is_expanded(directory));
    assert_eq!(explorer.selected_id(), Some(children[0]));
    assert!(explorer.visible_index(children[0]).is_some());

    explorer.set_filter("match");
    explorer.select(directory);
    explorer.select_first_child();
    explorer.ensure_selection();
    assert_eq!(explorer.selected_id(), Some(children[1]));

    // Matching the directory alone does not expose its nonmatching children.
    explorer.set_filter("directory");
    explorer.select(directory);
    explorer.select_first_child();
    assert_eq!(explorer.selected_id(), Some(directory));
}

#[test]
fn explorer_keeps_multi_roots_selectable() {
    let first = tempdir().expect("first");
    let second = tempdir().expect("second");
    let workspace = WorkspaceState::new(
        WorkspaceId(9),
        [
            (
                "first".into(),
                RootIdentity::capture(first.path()).expect("first identity"),
            ),
            (
                "second".into(),
                RootIdentity::capture(second.path()).expect("second identity"),
            ),
        ],
    )
    .expect("workspace");
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), workspace.root_revision());
    let roots = explorer.visible_ids().to_vec();
    assert_eq!(roots.len(), 2);
    explorer.move_selection(1);
    assert_eq!(explorer.selected_id(), Some(roots[1]));
    explorer.move_selection(-1);
    assert_eq!(explorer.selected_id(), Some(roots[0]));
}

#[test]
fn diagnostics_replace_by_owner_reject_stale_and_obey_bounds() {
    let owner = DiagnosticOwner::new("lsp.fake").expect("owner");
    let mut store = DiagnosticStore::new(3, 2);
    store
        .replace(
            owner.clone(),
            "file:///a".into(),
            Some(BufferRevision(1)),
            Some(BufferRevision(1)),
            vec![
                diagnostic("file:///a", 1, 0, "one"),
                diagnostic("file:///a", 1, 2, "two"),
                diagnostic("file:///a", 1, 4, "truncated"),
            ],
        )
        .expect("replace");
    assert_eq!(store.len(), 2);
    assert!(
        store
            .replace(
                owner.clone(),
                "file:///a".into(),
                Some(BufferRevision(1)),
                Some(BufferRevision(2)),
                Vec::new(),
            )
            .is_err()
    );
    assert_eq!(
        store
            .adjacent(
                "file:///a",
                ByteOffset(0),
                1,
                &[DiagnosticSeverity::Error, DiagnosticSeverity::Warning],
            )
            .map(|row| row.message),
        Some("two".into())
    );
    store
        .replace(
            owner.clone(),
            "file:///a".into(),
            Some(BufferRevision(2)),
            Some(BufferRevision(2)),
            vec![diagnostic("file:///a", 2, 0, "replacement")],
        )
        .expect("replace existing set");
    assert_eq!(store.len(), 1);
    assert_eq!(store.problems(None, None)[0].message, "replacement");
    store
        .replace(
            owner.clone(),
            "file:///b".into(),
            Some(BufferRevision(1)),
            Some(BufferRevision(1)),
            vec![diagnostic("file:///b", 1, 0, "other URI")],
        )
        .expect("same owner, other URI");
    let other_owner = DiagnosticOwner::new("lint.fake").expect("other owner");
    store
        .replace(
            other_owner.clone(),
            "file:///a".into(),
            Some(BufferRevision(2)),
            Some(BufferRevision(2)),
            vec![
                diagnostic("file:///a", 2, 2, "other owner"),
                diagnostic("file:///a", 2, 4, "over total limit"),
            ],
        )
        .expect("other owner, same URI");
    assert_eq!(store.len(), 3);
    let rows = store.problems(None, None);
    assert_eq!(
        rows.iter()
            .map(|row| (&row.owner, row.uri.as_str(), row.message.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (&owner, "file:///a", "replacement"),
            (&other_owner, "file:///a", "other owner"),
            (&owner, "file:///b", "other URI"),
        ]
    );
    store.clear_owner(&owner);
    assert_eq!(store.len(), 1);
    assert_eq!(store.problems(None, None), vec![rows[1].clone()]);
    store.clear_owner(&other_owner);
    assert!(store.is_empty());
}

#[test]
fn typing_selects_the_best_match_instead_of_keeping_the_old_row() {
    let mut picker = PickerState::new(4, "Worktrees".into(), 3);
    let item = |id: &str, label: &str, detail: &str| PickerItem {
        badge: String::new(),
        kind: crate::workspace::PickerItemKind::Entry,
        id: PickerItemId(id.into()),
        label: label.into(),
        detail: detail.into(),
        source: "fake".into(),
        action: PickerAction::None,
    };
    assert!(picker.append(
        4,
        [
            item(
                "new",
                "New worktree",
                "check out a branch in its own directory"
            ),
            item("o", "other", "")
        ],
        true
    ));
    assert_eq!(
        picker.selected_item().map(|item| item.id.0.as_str()),
        Some("new")
    );
    picker.insert_query("other");
    assert_eq!(
        picker.selected_item().map(|item| item.id.0.as_str()),
        Some("o")
    );
    picker.backspace_query();
    assert_eq!(
        picker.selected_item().map(|item| item.id.0.as_str()),
        Some("o")
    );
}

#[test]
fn picker_rejects_stale_streams_deduplicates_and_filters_stably() {
    let mut picker = PickerState::new(4, "References".into(), 3);
    assert!(!picker.append(3, Vec::new(), true));
    let item = |id: &str, label: &str| PickerItem {
        badge: String::new(),
        kind: crate::workspace::PickerItemKind::Entry,
        id: PickerItemId(id.into()),
        label: label.into(),
        detail: String::new(),
        source: "fake".into(),
        action: PickerAction::None,
    };
    assert!(picker.append(
        4,
        [
            item("a", "alpha"),
            item("b", "beta"),
            item("a", "duplicate")
        ],
        true
    ));
    let presentation = picker.presentation();
    assert_eq!(
        presentation
            .rows
            .iter()
            .map(|row| (row.id.0.as_str(), row.label.as_str()))
            .collect::<Vec<_>>(),
        vec![("a", "alpha"), ("b", "beta")]
    );
    picker.set_query("bt".into());
    assert_eq!(
        picker.selected_item().map(|item| item.id.0.as_str()),
        Some("b")
    );
    picker.insert_query("z");
    assert!(picker.selected_item().is_none());
    picker.backspace_query();
    assert_eq!(picker.presentation().rows.len(), 1);
}

#[test]
fn picker_filtering_keeps_matching_entries_under_their_sections() {
    let mut picker = PickerState::new(4, "Changes".into(), 20);
    let item = |id: &str, label: &str, kind| PickerItem {
        badge: String::new(),
        kind,
        id: PickerItemId(id.into()),
        label: label.into(),
        detail: String::new(),
        source: String::new(),
        action: PickerAction::None,
    };
    use PickerItemKind::{Entry, Section};
    assert!(picker.append(
        4,
        [
            item("draft", "Commit draft", Entry),
            item("conflicts", "Conflicts", Section),
            item("conflict", "other.rs", Entry),
            item("unstaged", "Unstaged", Section),
            item("unstaged-scattered", "a_b.rs", Entry),
            item("unstaged-exact", "ab.rs", Entry),
            item("staged", "Staged", Section),
            item("staged-exact", "ab.rs", Entry),
        ],
        true
    ));
    picker.insert_query("ab");
    let presentation = picker.presentation();
    assert_eq!(
        presentation
            .rows
            .iter()
            .map(|row| row.id.0.as_str())
            .collect::<Vec<_>>(),
        vec![
            "unstaged",
            "unstaged-exact",
            "unstaged-scattered",
            "staged",
            "staged-exact"
        ]
    );
    assert_eq!(picker.selected_item().unwrap().id.0, "unstaged-exact");
    picker.move_selection(1, false);
    picker.move_selection(1, false);
    assert_eq!(picker.selected_item().unwrap().id.0, "staged-exact");
    picker.set_query("staged".into());
    assert!(
        picker.presentation().rows.is_empty(),
        "headers alone are not matches"
    );
    picker.set_query(String::new());
    assert_eq!(picker.presentation().rows.len(), 8);
    assert_eq!(picker.selected_item().unwrap().id.0, "draft");
}

#[test]
fn picker_fuzzy_adjacency_uses_the_previous_unicode_character_end() {
    let adjacent = fuzzy_score("xéa", "xé za", "").unwrap();
    let scattered = fuzzy_score("xéa", "x éza", "").unwrap();
    assert!(adjacent > scattered);
    assert_eq!(fuzzy_score("xéa", "xéa z", ""), Some(1346));
}

#[test]
fn workspace_edit_proposal_validates_scope_overlap_staleness_and_rejection() {
    let directory = tempdir().expect("tempdir");
    let workspace = workspace(directory.path());
    let path = directory.path().join("a.txt");
    fs::write(&path, "abc").expect("write");
    let path = fs::canonicalize(path).expect("canonical");
    let snapshot = FileSnapshot {
        canonical_path: path.clone(),
        revision: Some(BufferRevision(9)),
        bytes: b"abc".to_vec(),
    };
    let draft = WorkspaceEditDraft {
        canonical_path: path.clone(),
        expected_revision: Some(BufferRevision(9)),
        edits: vec![TextEdit::new(
            ByteRange::new(ByteOffset(1), ByteOffset(2)).expect("range"),
            "Z",
        )],
    };
    let mut proposal = WorkspaceEditProposal::build(
        ProposalId(1),
        "rename".into(),
        &workspace,
        vec![draft.clone()],
        vec![snapshot.clone()],
    )
    .expect("proposal");
    assert_eq!(proposal.files[0].proposed_bytes, b"aZc");
    proposal.reject().expect("reject");
    assert_eq!(proposal.state, ProposalState::Rejected);
    assert!(proposal.accept().is_err());

    let outside = tempdir().expect("outside root");
    let outside_path = outside.path().join("outside.txt");
    fs::write(&outside_path, "abc").expect("outside file");
    let outside_path = fs::canonicalize(outside_path).expect("canonical outside path");
    assert!(matches!(
        WorkspaceEditProposal::build(
            ProposalId(4),
            "outside root".into(),
            &workspace,
            vec![WorkspaceEditDraft {
                canonical_path: outside_path.clone(),
                ..draft.clone()
            }],
            vec![FileSnapshot {
                canonical_path: outside_path.clone(),
                ..snapshot.clone()
            }],
        ),
        Err(WorkspaceError::PathOutsideRoots(path)) if path == outside_path
    ));

    let stale = WorkspaceEditDraft {
        expected_revision: Some(BufferRevision(8)),
        ..draft.clone()
    };
    assert!(
        WorkspaceEditProposal::build(
            ProposalId(2),
            "stale".into(),
            &workspace,
            vec![stale],
            vec![snapshot.clone()]
        )
        .is_err()
    );
    let overlap = WorkspaceEditDraft {
        edits: vec![
            TextEdit::new(
                ByteRange::new(ByteOffset(0), ByteOffset(2)).expect("range"),
                "x",
            ),
            TextEdit::new(
                ByteRange::new(ByteOffset(1), ByteOffset(3)).expect("range"),
                "y",
            ),
        ],
        ..draft
    };
    assert!(
        WorkspaceEditProposal::build(
            ProposalId(3),
            "overlap".into(),
            &workspace,
            vec![overlap],
            vec![snapshot]
        )
        .is_err()
    );
}

#[test]
fn nested_explorer_roots_keep_their_identity_and_children() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("sub");
    fs::create_dir(&nested).unwrap();
    let workspace = WorkspaceState::new(
        WorkspaceId(1),
        [
            ("outer".into(), RootIdentity::capture(dir.path()).unwrap()),
            ("inner".into(), RootIdentity::capture(&nested).unwrap()),
        ],
    )
    .unwrap();
    let revision = workspace.root_revision();
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), revision);
    let outer_path = &workspace.roots()[0].identity.canonical_path;
    let nested = &workspace.roots()[1].identity.canonical_path;
    let outer = explorer.id_for_path(outer_path).unwrap();
    let inner = explorer.id_for_path(nested).unwrap();
    assert!(explorer.begin_load(inner, 1));
    assert!(explorer.apply_load(
        inner,
        1,
        revision,
        vec![ExplorerChild {
            path: nested.join("file"),
            name: "file".into(),
            kind: ExplorerKind::File,
        }],
    ));
    let child = explorer.id_for_path(&nested.join("file")).unwrap();
    assert_eq!(explorer.node(inner).unwrap().children, vec![child]);
    assert_eq!(explorer.node(child).unwrap().parent, Some(inner));
    let before = explorer.node(inner).unwrap().clone();
    assert!(explorer.begin_load(outer, 2));
    assert!(explorer.apply_load(
        outer,
        2,
        revision,
        vec![ExplorerChild {
            path: nested.clone(),
            name: "sub".into(),
            kind: ExplorerKind::Directory,
        }],
    ));
    assert_eq!(explorer.node(inner), Some(&before));
    assert!(explorer.node(outer).unwrap().children.is_empty());
}

#[test]
fn explorer_refresh_preserves_a_later_collapse() {
    let dir = tempdir().unwrap();
    let workspace = workspace(dir.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 1);
    let root = explorer.selected_id().unwrap();
    assert!(explorer.begin_load(root, 1));
    assert!(explorer.apply_load(root, 1, 1, vec![]));
    assert!(explorer.begin_load(root, 2));
    explorer.toggle_loaded(root);
    assert!(!explorer.is_expanded(root));
    let path = workspace.roots()[0].identity.canonical_path.join("new.txt");
    assert!(explorer.apply_load(
        root,
        2,
        1,
        vec![ExplorerChild {
            path: path.clone(),
            name: "new.txt".into(),
            kind: ExplorerKind::File,
        }]
    ));
    let child = explorer.id_for_path(&path).expect("refreshed child");
    let node = explorer.node(root).unwrap();
    assert_eq!(node.load, ExplorerLoadState::Loaded);
    assert_eq!(node.children, vec![child]);
    assert_eq!(explorer.node(child).unwrap().name, "new.txt");
    assert!(!explorer.is_expanded(root));
}
