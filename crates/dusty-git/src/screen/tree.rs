//! The file tree shared by working changes, commit inspection and pull requests.
use super::Row;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Node {
    code: Option<String>,
    children: BTreeMap<OsString, Node>,
}

/// Directory identities are the deepest path in a compacted chain. Indentation is
/// display depth, not filesystem depth; consumers must not reconstruct it from `value`.
pub(super) fn rows(paths: BTreeMap<PathBuf, String>, collapsed: &BTreeSet<PathBuf>) -> Vec<Row> {
    let mut root = Node::default();
    for (path, code) in paths {
        let mut node = &mut root;
        for component in path.components() {
            node = node
                .children
                .entry(component.as_os_str().into())
                .or_default();
        }
        node.code = Some(code);
    }
    let mut rows = Vec::new();
    append(&root, Path::new(""), 0, collapsed, &mut rows);
    rows
}

fn append(
    parent: &Node,
    prefix: &Path,
    depth: usize,
    collapsed: &BTreeSet<PathBuf>,
    rows: &mut Vec<Row>,
) {
    for (name, child) in &parent.children {
        let mut path = prefix.join(name);
        let mut label = name.to_string_lossy().into_owned();
        let mut node = child;
        if node.code.is_none() {
            while !collapsed.contains(&path) && node.children.len() == 1 {
                let (name, next) = node.children.first_key_value().expect("one child");
                if next.code.is_some() {
                    break;
                }
                path.push(name);
                label.push('/');
                label.push_str(&name.to_string_lossy());
                node = next;
            }
            let closed = collapsed.contains(&path);
            rows.push(Row {
                label: format!(
                    "{}{} {label}/",
                    "  ".repeat(depth),
                    if closed { "▸" } else { "▾" },
                ),
                value: path.clone(),
                directory: true,
                indent: depth * 2,
            });
            if !closed {
                append(node, &path, depth + 1, collapsed, rows);
            }
        } else {
            let code = node.code.as_deref().unwrap_or_default();
            rows.push(Row {
                label: format!(
                    "{}{}{label}",
                    "  ".repeat(depth),
                    if code.is_empty() {
                        String::new()
                    } else {
                        format!("{code} ")
                    },
                ),
                value: path,
                directory: false,
                indent: depth * 2,
            });
        }
    }
}

/// The visible parent may be several filesystem components above a compacted row.
pub(super) fn parent(rows: &[Row], path: &Path) -> Option<PathBuf> {
    rows.iter()
        .filter(|row| row.directory && row.value != path && path.starts_with(&row.value))
        .max_by_key(|row| row.value.components().count())
        .map(|row| row.value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_chains_keep_real_paths_and_display_depth() {
        let paths = BTreeMap::from([
            (PathBuf::from("src/git/screen/a.rs"), " M".into()),
            (PathBuf::from("src/git/screen/b.rs"), " A".into()),
        ]);
        let open = rows(paths.clone(), &BTreeSet::new());
        assert_eq!(open.len(), 3);
        assert_eq!(open[0].label, "▾ src/git/screen/");
        assert_eq!(open[0].value, Path::new("src/git/screen"));
        assert_eq!(open[1].indent, 2);
        assert_eq!(open[1].value, Path::new("src/git/screen/a.rs"));
        assert_eq!(parent(&open, &open[1].value), Some(open[0].value.clone()));
        let closed = rows(paths.clone(), &BTreeSet::from([open[0].value.clone()]));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].label, "▸ src/git/screen/");
        assert_eq!(closed[0].value, open[0].value);
        let ancestor = rows(paths, &BTreeSet::from([PathBuf::from("src")]));
        assert_eq!(ancestor.len(), 1);
        assert_eq!(ancestor[0].value, Path::new("src"));
    }

    #[test]
    fn branching_and_files_stop_compaction() {
        let paths = BTreeMap::from([
            (PathBuf::from("src/lib.rs"), String::new()),
            (PathBuf::from("src/git/screen/a.rs"), String::new()),
            (PathBuf::from("src/git/screen/b.rs"), String::new()),
        ]);
        let rows = rows(paths, &BTreeSet::new());
        assert_eq!(rows[0].label, "▾ src/");
        assert_eq!(rows[1].label, "  ▾ git/screen/");
        assert_eq!(rows[2].indent, 4);
        assert_eq!(parent(&rows, &rows[1].value), Some(PathBuf::from("src")));
        assert!(rows.iter().any(|row| row.value == Path::new("src/lib.rs")));
    }
}
