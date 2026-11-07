//! What the Settings page lists: each setting's section, title and description, from
//! `assets/defaults/settings.toml`, and how one setting is changed in the user's config.toml.

use std::path::Path;

use std::sync::OnceLock;

use serde::Deserialize;

const CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/settings.toml"
));
const SETUP: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/setup.toml"
));

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsCatalog {
    /// Tables edited in config.toml itself rather than one row per entry.
    pub collections: Vec<String>,
    #[serde(rename = "section")]
    pub sections: Vec<SettingsSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSection {
    pub title: String,
    /// Keys from other sections shown here again, for a section of favourites.
    #[serde(default)]
    pub common: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingInfo>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingInfo {
    pub key: String,
    #[serde(default)]
    title: Option<String>,
    pub about: String,
    #[serde(default)]
    pub choices: Vec<String>,
    /// A configuration table whose entry names are the choices, such as `agents.profiles`.
    #[serde(default)]
    pub choices_from: Option<String>,
    /// The first choice, which leaves the setting empty, such as "Ask every time".
    #[serde(default)]
    pub empty: Option<String>,
    /// The change applies the next time Dust starts.
    #[serde(default)]
    pub restart: bool,
}

impl SettingInfo {
    /// The given title, or one made from the key the way VS Code does: `editor.git_signs.enabled`
    /// is "Git Signs: Enabled".
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        let segments: Vec<&str> = self.key.split('.').collect();
        let name = humanized(segments.last().copied().unwrap_or_default());
        match segments.len() {
            0..=2 => name,
            length => format!("{}: {name}", humanized(segments[length - 2])),
        }
    }
}

fn humanized(name: &str) -> String {
    let mut words = Vec::new();
    let mut unit = "";
    for word in name.split('_').filter(|word| !word.is_empty()) {
        match word {
            "ms" => unit = " (ms)",
            "lsp" | "ui" | "url" | "id" | "vfs" => words.push(word.to_ascii_uppercase()),
            "utf8" => words.push("UTF-8".into()),
            _ => {
                let mut characters = word.chars();
                let first = characters
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>());
                words.push(first.unwrap_or_default() + characters.as_str());
            }
        }
    }
    words.join(" ") + unit
}

impl SettingsCatalog {
    #[must_use]
    pub fn setting(&self, key: &str) -> Option<&SettingInfo> {
        self.sections
            .iter()
            .flat_map(|section| &section.settings)
            .find(|setting| setting.key == key)
    }
}

/// The bundled catalog; a test keeps it parsing and complete.
#[must_use]
pub fn settings_catalog() -> &'static SettingsCatalog {
    static PARSED: OnceLock<SettingsCatalog> = OnceLock::new();
    PARSED.get_or_init(|| toml::from_str(CATALOG).expect("the settings catalog must parse"))
}

/// The questions Dust asks on first start, from `assets/defaults/setup.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCatalog {
    #[serde(rename = "page")]
    pub pages: Vec<SetupPageInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPageInfo {
    pub title: String,
    pub question: String,
    /// What the small editor beside the choices shows open: `layout`, `terminal`, `agent` or
    /// `leader`.
    #[serde(default)]
    pub preview: String,
    /// A configuration table whose entry names are more choices, each setting `key` to its name.
    #[serde(default)]
    pub choices_from: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default, rename = "choice")]
    pub choices: Vec<SetupChoiceInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupChoiceInfo {
    pub label: String,
    pub about: String,
    pub settings: std::collections::BTreeMap<String, toml::Value>,
}

/// The bundled setup questions; a test keeps them parsing and their keys real.
#[must_use]
pub fn setup_catalog() -> &'static SetupCatalog {
    static PARSED: OnceLock<SetupCatalog> = OnceLock::new();
    PARSED.get_or_init(|| toml::from_str(SETUP).expect("the setup questions must parse"))
}

/// A setting's bundled default.
#[must_use]
pub fn default_setting(key: &str) -> Option<toml::Value> {
    crate::config::load::bundled_setting(key)
}

/// Set one setting in the user's config.toml, or remove it with `None` so the default applies,
/// leaving the rest of the file and its comments as they were. The new file is checked like a
/// reload would check it before it is written, so a bad value never reaches the disk.
pub fn write_user_setting(
    path: &Path,
    key: &str,
    value: Option<toml::Value>,
) -> Result<(), String> {
    let file = UserFile::read(path)?;
    let edited = edited_source(&file.source, key, value)?;
    if edited.len() as u64 > crate::config::load::WATCH_FINGERPRINT_BYTE_LIMIT {
        return Err("config.toml exceeds the configuration byte limit".into());
    }
    // A value of the wrong type fails in the deserializer, whose message names Rust types.
    crate::config::load::settings_from_source(&edited).map_err(|error| {
        if error
            .lines()
            .next()
            .is_some_and(|line| line.contains("expected"))
        {
            format!("{key} can't be set to that value")
        } else {
            error
        }
    })?;
    file.write(&edited)
}

/// A user configuration file read for editing, following a symlink, and written back only if
/// nothing changed it in between.
pub(super) struct UserFile {
    path: std::path::PathBuf,
    pub(super) source: String,
    expectation: crate::files::WriteExpectation,
}

impl UserFile {
    pub(super) fn read(path: &Path) -> Result<Self, String> {
        let path = match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path)
                .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?,
            Ok(_) => path.to_path_buf(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        let (source, expectation) = match std::fs::read_to_string(&path) {
            Ok(source) => {
                let expectation = crate::files::WriteExpectation::Exact(source.as_bytes().to_vec());
                (source, expectation)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (String::new(), crate::files::WriteExpectation::Missing)
            }
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        Ok(Self {
            path,
            source,
            expectation,
        })
    }

    pub(super) fn write(&self, edited: &str) -> Result<(), String> {
        let path = &self.path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        crate::files::checked_atomic_write(path, edited.as_bytes(), &self.expectation)
            .map_err(|error| format!("cannot save {}: {error}", path.display()))
    }
}

pub(super) fn edited_source(
    source: &str,
    key: &str,
    value: Option<toml::Value>,
) -> Result<String, String> {
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("config.toml has a mistake to fix first: {error}"))?;
    let segments =
        crate::config::config_key_segments(key).ok_or_else(|| format!("bad key {key}"))?;
    let Some(value) = value else {
        if let Some(comments) = remove_setting(document.as_table_mut(), &segments) {
            let trailing = document.trailing().as_str().unwrap_or("").to_owned();
            document.set_trailing(format!("{trailing}{comments}"));
        }
        return Ok(document.to_string());
    };
    let value = value
        .to_string()
        .parse::<toml_edit::Value>()
        .map_err(|error| error.to_string())?;
    let (leaf, parents) = segments.split_last().ok_or("empty key")?;
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for segment in parents {
        if table.get(segment).is_none() {
            let mut created = toml_edit::Table::new();
            created.set_implicit(true);
            table.insert(segment, toml_edit::Item::Table(created));
        }
        table = table
            .get_mut(segment)
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| format!("`{segment}` in config.toml is not a table"))?;
    }
    match table.get_mut(leaf).and_then(toml_edit::Item::as_value_mut) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = value;
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(leaf, toml_edit::Item::Value(value));
        }
    }
    Ok(document.to_string())
}

/// Remove the key, and the tables it leaves empty. The key's comments stay: they move to the
/// next key, or under the table's header when the key was its last. Returns the comments that
/// found no place at this level.
fn remove_setting(table: &mut dyn toml_edit::TableLike, segments: &[String]) -> Option<String> {
    let (first, rest) = segments.split_first()?;
    if rest.is_empty() {
        let comments = setting_comments(table, first);
        let next = table
            .iter()
            .map(|(key, _)| key.to_owned())
            .skip_while(|key| key != first)
            .nth(1);
        table.remove(first)?;
        let comments = comments?;
        return match next.as_deref().and_then(|next| table.key_mut(next)) {
            Some(mut key) => {
                let prefix = key
                    .leaf_decor()
                    .prefix()
                    .and_then(toml_edit::RawString::as_str);
                let prefix = format!("{comments}{}", prefix.unwrap_or(""));
                key.leaf_decor_mut().set_prefix(prefix);
                None
            }
            None => Some(comments),
        };
    }
    let (empty, orphan) = match table.get_mut(first) {
        Some(toml_edit::Item::Table(inner)) if !inner.is_implicit() && !inner.is_dotted() => {
            if let Some(comments) = remove_setting(inner, rest) {
                let suffix = inner
                    .decor()
                    .suffix()
                    .and_then(toml_edit::RawString::as_str);
                let suffix = format!("{}\n{}", suffix.unwrap_or(""), comments.trim_end());
                inner.decor_mut().set_suffix(suffix);
            }
            // A table header with a comment on or above it stays, so the comment is not lost.
            let commented = [inner.decor().prefix(), inner.decor().suffix()]
                .into_iter()
                .flatten()
                .filter_map(toml_edit::RawString::as_str)
                .any(|text| text.contains('#'));
            (inner.is_empty() && !commented, None)
        }
        Some(item) => match item.as_table_like_mut() {
            Some(inner) => {
                let orphan = remove_setting(inner, rest);
                (inner.is_empty() && orphan.is_none(), orphan)
            }
            None => (false, None),
        },
        None => (false, None),
    };
    if empty {
        return remove_setting(table, &segments[..1]);
    }
    orphan
}

/// The comment lines above a key and the comment after its value, each ending in a newline.
fn setting_comments(table: &dyn toml_edit::TableLike, key: &str) -> Option<String> {
    let above = table
        .key(key)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or("");
    let after = table
        .get(key)
        .and_then(toml_edit::Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or("");
    let lines = above
        .lines()
        .chain(after.lines())
        .map(str::trim)
        .filter(|line| line.starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    (!lines.is_empty()).then_some(lines)
}
