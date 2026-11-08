//! Tree tests for explorer navigation and filesystem changes.

use crate::files::RootIdentity;
use crate::workspace::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

use crate::app::explorer::tree::{ExplorerChild, ExplorerKind, ExplorerLoadState, ExplorerState};

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
fn explorer_state_is_lazy_stable_and_rejects_stale_loads() {
    let root = tempdir().expect("root");
    let workspace = workspace(root.path());
    let mut explorer = ExplorerState::default();
    explorer.reset_roots(workspace.roots(), 3);
    let root_revision = explorer.projection_revision();
    let root_id = explorer.selected_node().expect("selected root").id;
    assert_eq!(explorer.visible_ids(), &[root_id]);
    assert!(explorer.begin_load(root_id, 9));
    let loading_revision = explorer.projection_revision();
    assert!(loading_revision > root_revision);
    assert!(!explorer.apply_load(root_id, 8, 3, Vec::new()));
    assert!(!explorer.apply_load(root_id, 9, 2, Vec::new()));
    assert_eq!(explorer.projection_revision(), loading_revision);
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
    assert!(explorer.projection_revision() > loading_revision);
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
    let loaded_revision = explorer.projection_revision();
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
    let nested_revision = explorer.projection_revision();
    explorer.move_selection(-1);
    assert_eq!(explorer.selected_id(), Some(root_id));
    assert_eq!(explorer.projection_revision(), nested_revision);

    explorer.select(nested_id);
    explorer.collapse_or_parent();
    assert_eq!(explorer.selected_id(), Some(child_id));
    explorer.collapse_or_parent();
    let collapsed_revision = explorer.projection_revision();
    assert_eq!(explorer.selected_id(), Some(child_id));
    assert_eq!(explorer.visible_ids(), &[root_id, child_id]);
    explorer.collapse_or_parent();
    assert_eq!(explorer.selected_id(), Some(root_id));
    assert_eq!(explorer.projection_revision(), collapsed_revision);

    explorer.select(root_id);
    explorer.toggle_loaded(root_id);
    assert!(explorer.projection_revision() > loaded_revision);
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
