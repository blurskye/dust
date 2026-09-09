#![forbid(unsafe_code)]

mod load;
mod themes;
#[cfg(test)]
pub(crate) use load::setting_spans;
pub use load::{
    ConfigurationWorker, ConfigurationWorkerError, ConfigurationWorkerResult, load_bundled_config,
    load_config, load_keymaps, load_user_config, user_config_path, user_dashboard_path,
    user_keymaps_path, user_state_directory, user_theme_path,
};
pub(crate) use themes::{
    ThemeCatalog, ThemeChoice, ThemeRead, ThemeReadSource, theme_display_name,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
mod settings;
pub use settings::*;
mod validation;
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "current_config_schema")]
    pub schema_version: u32,
    pub editor: EditorConfig,
    pub ui: UiConfig,
    pub startup: StartupConfig,
    pub input: InputConfig,
    pub search: SearchConfig,
    pub history: HistoryConfig,
    pub recovery: RecoveryConfig,
    pub config_watch: ConfigWatchConfig,
    pub runtime: RuntimeConfig,
    pub lsp: LspConfig,
    pub observability: ObservabilityConfig,
    pub persistence: PersistenceConfig,
    pub processes: ProcessConfig,
    #[serde(default)]
    pub packages: PackagesConfig,
    pub plugins: PluginsConfig,
    pub debug: DebugConfig,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    pub syntax: SyntaxConfig,
    pub terminals: TerminalsConfig,
    #[serde(default)]
    pub windows: WindowsConfig,
    pub workspace: WorkspaceConfig,
    pub messages: BTreeMap<String, String>,
    pub command_labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeyGroupEntry {
    pub id: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub disable: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeymapEntry {
    pub id: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub keys: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub unmap: bool,
    #[serde(default)]
    pub disable: bool,
    #[serde(default)]
    pub pass_through: bool,
}

const fn current_config_schema() -> u32 {
    crate::api::CONFIG_SCHEMA_VERSION
}
pub fn migrate_config_source(source: &str) -> Result<(String, bool), String> {
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| error.to_string())?;
    let version = document
        .get("schema_version")
        .and_then(toml_edit::Item::as_integer)
        .unwrap_or(0);
    if version < 0
        || u32::try_from(version)
            .ok()
            .is_none_or(|version| version > crate::api::CONFIG_SCHEMA_VERSION)
    {
        return Err(format!("unsupported future configuration schema {version}"));
    }
    let changed = version != i64::from(crate::api::CONFIG_SCHEMA_VERSION);
    if changed {
        document["schema_version"] = toml_edit::value(i64::from(crate::api::CONFIG_SCHEMA_VERSION));
    }
    Ok((document.to_string(), changed))
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardActionConfig {
    pub command: String,
    pub label: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_true")]
    pub show_binding: bool,
    #[serde(default)]
    pub arguments: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardConfig {
    pub show_identity: bool,
    pub show_version: bool,
    pub show_path: bool,
    pub identity: String,
    pub version_format: String,
    pub resize_message: String,
    pub unbound_label: String,
    pub key_format: String,
    pub content_width: u16,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub block_height: u16,
    pub identity_row: u16,
    pub version_row: u16,
    pub path_row: u16,
    pub actions_row: u16,
    pub action_key_width: u16,
    pub action_left_offset: u16,
    pub compact_left: u16,
    pub compact_top: u16,
    #[serde(default)]
    pub action: Vec<DashboardActionConfig>,
}

impl DashboardConfig {
    fn validate(&self) -> Result<(), String> {
        if self.content_width == 0
            || self.minimum_width == 0
            || self.minimum_height == 0
            || self.block_height == 0
            || self.action_key_width == 0
        {
            return Err("dashboard dimensions must be positive".into());
        }
        if !self.version_format.contains("{version}") {
            return Err("dashboard version_format must contain `{version}`".into());
        }
        if !self.key_format.contains("{key}") {
            return Err("dashboard key_format must contain `{key}`".into());
        }
        if self.content_width > 1_000
            || self.block_height > 1_000
            || self.action.len() > 1_000
            || self
                .identity
                .chars()
                .chain(self.resize_message.chars())
                .any(char::is_control)
        {
            return Err("dashboard presentation exceeds safe bounds".into());
        }
        Ok(())
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            show_identity: true,
            show_version: true,
            show_path: true,
            identity: "D U S T".into(),
            version_format: "v{version}".into(),
            resize_message: "Dust — resize terminal".into(),
            unbound_label: "unbound".into(),
            key_format: "[{key}]".into(),
            content_width: 70,
            minimum_width: 20,
            minimum_height: 7,
            block_height: 9,
            identity_row: 0,
            version_row: 1,
            path_row: 3,
            actions_row: 5,
            action_key_width: 12,
            action_left_offset: 2,
            compact_left: 1,
            compact_top: 1,
            action: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedKeymaps {
    pub entries: Vec<KeymapEntry>,
    pub groups: Vec<KeyGroupEntry>,
    pub user_spans: Vec<std::ops::Range<usize>>,
    pub user_group_spans: Vec<std::ops::Range<usize>>,
    pub bundled_count: usize,
    pub bundled_group_count: usize,
    pub path: PathBuf,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    pub user_configuration: bool,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub config: Config,
    pub path: PathBuf,
    pub diagnostic: Option<String>,
    pub keymaps: LoadedKeymaps,
    pub dashboard: DashboardConfig,
    pub dashboard_path: PathBuf,
    pub dashboard_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub theme: ThemeConfig,
    pub theme_path: PathBuf,
    pub theme_spans: BTreeMap<String, std::ops::Range<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigExplanation {
    pub key: String,
    pub value: String,
    pub source: String,
    pub contributors: Vec<String>,
}

pub(crate) fn config_key_segments(key: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in key.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '.' && !quoted {
            if current.is_empty() {
                return None;
            }
            segments.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    if quoted || escaped || current.is_empty() {
        return None;
    }
    segments.push(current);
    Some(segments)
}

impl LoadedConfig {
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<ConfigExplanation> {
        let segments = config_key_segments(key)?;
        let canonical = segments.join(".");
        let mut value = toml::Value::try_from(&self.config).ok()?;
        for segment in &segments {
            value = value.get(segment)?.clone();
        }
        let bundled = !self.user_setting_keys.contains(&canonical);
        let user_source = self.user_setting_spans.get(&canonical).map_or_else(
            || self.path.display().to_string(),
            |span| format!("{}:bytes {}..{}", self.path.display(), span.start, span.end),
        );
        Some(ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: if bundled {
                "bundled defaults".into()
            } else {
                user_source.clone()
            },
            contributors: if bundled {
                vec!["bundled defaults".into()]
            } else {
                vec!["bundled defaults".into(), user_source]
            },
        })
    }
}

#[cfg(test)]
mod tests;
