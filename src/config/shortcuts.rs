//! What the Keyboard Shortcuts page lists: every key binding in effect from
//! `assets/defaults/keymaps.toml` and the user's keymaps.toml, and how one binding is changed in
//! keymaps.toml.

use std::collections::HashMap;

use std::path::Path;

use serde::Deserialize;

use crate::config::KeymapEntry;
use crate::config::catalog::UserFile;

/// A binding as the user's files leave it.
#[derive(Clone, Debug)]
pub struct DeclaredKeymap {
    pub entry: KeymapEntry,
    /// The user's keymaps.toml sets it.
    pub user: bool,
}

#[derive(Deserialize)]
struct KeymapFile {
    #[serde(default)]
    keymap: Vec<KeymapEntry>,
    /// Allowed in the file, but not shortcuts.
    #[serde(default, rename = "key_group")]
    _key_groups: Vec<toml::Value>,
}

/// Every binding in declaration order. A user entry replaces the bundled one with its id and
/// `disable = true` removes it, as the keymap compiler does; new user entries come last. A
/// keymaps.toml that doesn't parse is left out, as a reload leaves it out.
#[must_use]
pub fn declared_keymaps(user: Option<&Path>) -> Vec<DeclaredKeymap> {
    let parse = |source: &str| {
        toml::from_str::<KeymapFile>(source)
            .map(|file| file.keymap)
            .unwrap_or_default()
    };
    let mut declared: Vec<Option<DeclaredKeymap>> = parse(crate::config::load::BUNDLED_KEYMAPS)
        .into_iter()
        .map(|entry| Some(DeclaredKeymap { entry, user: false }))
        .collect();
    let mut slots: HashMap<String, usize> = declared
        .iter()
        .enumerate()
        .filter_map(|(slot, keymap)| Some((keymap.as_ref()?.entry.id.clone(), slot)))
        .collect();
    let user = user
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|source| parse(&source))
        .unwrap_or_default();
    for entry in user {
        let keymap = (!entry.disable).then(|| DeclaredKeymap {
            entry: entry.clone(),
            user: true,
        });
        match slots.get(&entry.id) {
            Some(&slot) => declared[slot] = keymap,
            None => {
                slots.insert(entry.id, declared.len());
                declared.push(keymap);
            }
        }
    }
    declared.into_iter().flatten().collect()
}

/// Bind `entry` to `keys` in the user's keymaps.toml: the user's entry with that id gets the new
/// keys, or a copy of `entry` with them is added, which replaces the bundled one.
pub fn write_user_keymap(path: &Path, entry: &KeymapEntry, keys: &str) -> Result<(), String> {
    edit_user_keymaps(path, |document| {
        if let Some(item) = document.get_mut("keymap") {
            let tables = item.as_array_of_tables_mut().ok_or(NOT_TABLES)?;
            let existing = tables
                .iter()
                .position(|table| table.get("id").and_then(|id| id.as_str()) == Some(&entry.id));
            if let Some(table) = existing.and_then(|index| tables.get_mut(index)) {
                table["keys"] = toml_edit::value(keys);
                return Ok(document.to_string());
            }
        }
        let mut table = toml_edit::Table::new();
        table["id"] = toml_edit::value(&entry.id);
        if let Some(context) = &entry.context {
            table["context"] = toml_edit::value(context);
        }
        if !entry.mode.is_empty() {
            table["mode"] = toml_edit::value(&entry.mode);
        }
        table["keys"] = toml_edit::value(keys);
        if let Some(command) = &entry.command {
            table["command"] = toml_edit::value(command);
        }
        if !entry.arguments.is_empty() {
            let mut arguments = toml_edit::InlineTable::new();
            for (name, value) in &entry.arguments {
                let value = value
                    .to_string()
                    .parse::<toml_edit::Value>()
                    .map_err(|error| error.to_string())?;
                arguments.insert(name, value);
            }
            table["arguments"] = toml_edit::value(arguments);
        }
        if let Some(desc) = &entry.desc {
            table["desc"] = toml_edit::value(desc);
        }
        let mut list = toml_edit::ArrayOfTables::new();
        list.push(table);
        let mut added = toml_edit::DocumentMut::new();
        added.insert("keymap", toml_edit::Item::ArrayOfTables(list));
        // Added at the end as text, so comments at the top of the file stay at the top.
        let source = document.to_string();
        let gap = match source.as_str() {
            "" => "",
            source if source.ends_with('\n') => "\n",
            _ => "\n\n",
        };
        Ok(format!("{source}{gap}{added}"))
    })
}

/// Remove the user's entry with this id, so the bundled binding applies again. Comments
/// written above it stay where they were.
pub fn remove_user_keymap(path: &Path, id: &str) -> Result<(), String> {
    edit_user_keymaps(path, |document| {
        let Some(item) = document.get_mut("keymap") else {
            return Ok(document.to_string());
        };
        let tables = item.as_array_of_tables_mut().ok_or(NOT_TABLES)?;
        let Some(index) = tables
            .iter()
            .position(|table| table.get("id").and_then(|value| value.as_str()) == Some(id))
        else {
            return Ok(document.to_string());
        };
        let comments: String = tables
            .get(index)
            .and_then(|table| table.decor().prefix())
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .map(|line| format!("{line}\n"))
            .collect();
        tables.remove(index);
        if index < tables.len() {
            if let Some(next) = tables.get_mut(index) {
                let prefix = next
                    .decor()
                    .prefix()
                    .and_then(|prefix| prefix.as_str())
                    .unwrap_or_default()
                    .to_owned();
                next.decor_mut().set_prefix(format!("{comments}{prefix}"));
            }
        } else {
            let trailing = document.trailing().as_str().unwrap_or_default().to_owned();
            document.set_trailing(format!("{comments}{trailing}"));
        }
        Ok(document.to_string())
    })
}

const NOT_TABLES: &str = "keymaps.toml has `keymap` that isn't a list of [[keymap]] tables";

/// Edit keymaps.toml, keeping its comments and everything else, and check the result parses as
/// a reload would before it is written.
fn edit_user_keymaps(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<String, String>,
) -> Result<(), String> {
    let file = UserFile::read(path)?;
    let mut document = file
        .source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("keymaps.toml has a mistake to fix first: {error}"))?;
    let edited = edit(&mut document)?;
    if let Some(diagnostic) =
        crate::config::load::parse_keymaps(&edited, path.to_path_buf()).diagnostic
    {
        return Err(diagnostic);
    }
    file.write(&edited)
}
