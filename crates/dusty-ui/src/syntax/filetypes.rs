//! Neovim's filetype table (file extensions and names to filetypes), from the pinned
//! snapshot the bundled Mason catalog carries. It names the languages that Dusty's own
//! extension table leaves out, such as `flake.lock` as JSON.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

const CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/tools/mason-lsp-catalog.json"
));

/// The catalog's `filetypes` table; the rest of the catalog belongs to the tool installer.
#[derive(Debug, Deserialize)]
struct Catalog {
    filetypes: FiletypeTable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FiletypeTable {
    extensions: BTreeMap<String, String>,
    filenames: BTreeMap<String, String>,
}

fn table() -> &'static FiletypeTable {
    static TABLE: OnceLock<FiletypeTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let catalog: Catalog =
            serde_json::from_str(CATALOG).expect("bundled Mason filetype table parses");
        validate(&catalog.filetypes).expect("bundled Mason filetype table validates");
        catalog.filetypes
    })
}

fn bounded_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16 * 1024 && !value.chars().any(char::is_control)
}

fn validate(table: &FiletypeTable) -> Result<(), String> {
    for (extension, filetype) in &table.extensions {
        if extension.len() > 128
            || !bounded_text(extension)
            || extension.contains('.')
            || !bounded_text(filetype)
        {
            return Err("Mason filetype extension mapping is invalid".into());
        }
    }
    for (filename, filetype) in &table.filenames {
        if filename.len() > 255
            || !bounded_text(filename)
            || filename.contains('/')
            || filename.contains('\\')
            || !bounded_text(filetype)
        {
            return Err("Mason filetype filename mapping is invalid".into());
        }
    }
    Ok(())
}

/// Neovim's filetype for `path`: its exact file name first, then its extension as written,
/// then its extension in lowercase.
#[must_use]
pub fn mason_filetype_for_path(path: &Path) -> Option<&'static str> {
    let table = table();
    if let Some(filename) = path.file_name().and_then(|name| name.to_str())
        && let Some(filetype) = table.filenames.get(filename)
    {
        return Some(filetype);
    }
    let extension = path.extension()?.to_str()?;
    table
        .extensions
        .get(extension)
        .or_else(|| table.extensions.get(&extension.to_ascii_lowercase()))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_filetype_table_is_complete() {
        let table = table();
        assert_eq!(table.extensions.len(), 1_231);
        assert_eq!(table.filenames.len(), 407);
    }
}
