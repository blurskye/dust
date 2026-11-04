#![forbid(unsafe_code)]

mod catalog;
mod load;
mod themes;
pub use catalog::{
    SettingInfo, SettingsCatalog, SettingsSection, default_setting, settings_catalog,
    write_user_setting,
};
#[cfg(test)]
pub(crate) use load::setting_spans;
pub use load::{
    ConfigurationWorker, ConfigurationWorkerError, ConfigurationWorkerResult, load_bundled_config,
    load_config, load_keymaps, load_user_config, user_config_path, user_dashboard_path,
    user_keymaps_path, user_state_directory, user_theme_path,
};
pub(crate) use themes::{
    ThemeCatalog, ThemeChoice, ThemeRead, ThemeReadSource, snapshot as theme_snapshot,
    theme_display_name,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
mod settings;
pub use settings::{
    AgentConnection, AgentProfileConfig, AgentsConfig, AmbiguousWidth, BorderStyle,
    BufferLineConfig, ClipboardSync, ColorMode, CommandLineConfig, CursorConfig, CursorShape,
    DebugAdapterConfig, DebugConfig, DebugRequestKind, DebugSessionConfig,
    DocumentationPopupConfig, EditorConfig, EditorViewportConfig, ExplorerConfig, ExplorerGlyphSet,
    ExplorerGlyphs, ExplorerIconMode, FinalNewlinePolicy, GitSignsConfig, HighlightDefinition,
    IndentGuideConfig, InlayHintLevel, InputConfig, KeyHintConfig, KeymapPolicyConfig,
    LargeFileConfig, LineEndingPolicy, LineNumbers, LspConfig, LspProgressConfig, MarkdownCallout,
    MarkdownConfig, MarkdownGlyphs, PersistenceConfig, PluginEntryConfig, PluginsConfig,
    ProblemMatcherConfig, PromptTextConfig, RecoveryConfig, RegisterConfig, SearchConfig,
    StartupConfig, StatusLineConfig, SyntaxConfig, TaskConfig, TaskKind, TerminalPanelConfig,
    TerminalProfileConfig, TerminalsConfig, ThemeAttribute, ThemeColor, ThemeConfig, ThemeStyle,
    ThemeStyles, ToolManagerConfig, UiConfig, WindowPlacementConfig, WindowRuleConfig,
    WindowsConfig, WorkspaceConfig, WorkspaceSearchConfig,
};
mod validation;

pub(crate) const CONFIG_SCHEMA_VERSION: u32 = 1;

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
    pub recovery: RecoveryConfig,
    pub lsp: LspConfig,
    pub persistence: PersistenceConfig,
    pub plugins: PluginsConfig,
    pub debug: DebugConfig,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    pub syntax: SyntaxConfig,
    pub terminals: TerminalsConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub windows: WindowsConfig,
    pub workspace: WorkspaceConfig,
    pub messages: BTreeMap<String, String>,
    pub command_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub command_palette: CommandPaletteConfig,
}

/// What the command palette (F1) lists and how it names the rows.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPaletteConfig {
    /// Row category by command id prefix; the longest matching prefix wins.
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
    /// Command id prefixes left out of the palette: plumbing only keys use.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Command line text a row opens with, for commands that need typed text.
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
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
    /// Short label shown in key hints (which-key). Falls back to the command description.
    #[serde(default)]
    pub desc: Option<String>,
}

const fn current_config_schema() -> u32 {
    CONFIG_SCHEMA_VERSION
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
            .is_none_or(|version| version > CONFIG_SCHEMA_VERSION)
    {
        return Err(format!("unsupported future configuration schema {version}"));
    }
    let changed = version != i64::from(CONFIG_SCHEMA_VERSION);
    if changed {
        document["schema_version"] = toml_edit::value(i64::from(CONFIG_SCHEMA_VERSION));
    }
    Ok((document.to_string(), changed))
}

/// Apply a migration to the same file that was read, preserving configuration symlinks.
pub fn apply_config_migration(
    path: &std::path::Path,
    source: &str,
    migrated: &str,
) -> Result<(), crate::files::VfsError> {
    let target = path
        .canonicalize()
        .map_err(|source| crate::files::VfsError::Io {
            operation: "resolve migration destination",
            path: path.to_path_buf(),
            source,
        })?;
    crate::files::checked_atomic_write(
        &target,
        migrated.as_bytes(),
        &crate::files::WriteExpectation::Exact(source.as_bytes().to_vec()),
    )
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardActionConfig {
    pub command: String,
    pub label: String,
    /// A one-cell glyph shown before the label; empty for none.
    #[serde(default)]
    pub icon: String,
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
    /// Big block letters shown on top; `identity` stands in when the screen is too small for
    /// them, or when this is empty.
    #[serde(default)]
    pub logo: String,
    pub show_identity: bool,
    pub identity: String,
    pub show_version: bool,
    pub version_format: String,
    pub show_path: bool,
    pub resize_message: String,
    /// Width of the action rows; the key sits at their right end.
    pub content_width: u16,
    pub minimum_width: u16,
    pub minimum_height: u16,
    #[serde(default)]
    pub action: Vec<DashboardActionConfig>,
}

impl DashboardConfig {
    fn validate(&self) -> Result<(), String> {
        if self.content_width == 0 || self.minimum_width == 0 || self.minimum_height == 0 {
            return Err("dashboard dimensions must be positive".into());
        }
        if !self.version_format.contains("{version}") {
            return Err("dashboard version_format must contain `{version}`".into());
        }
        if self.content_width > 1_000
            || self.action.len() > 1_000
            || self.logo.lines().count() > 64
            || self.logo.chars().any(|c| c.is_control() && c != '\n')
            || self
                .identity
                .chars()
                .chain(self.resize_message.chars())
                .chain(self.action.iter().flat_map(|action| action.icon.chars()))
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
            logo: String::new(),
            show_identity: true,
            identity: "D U S T".into(),
            show_version: true,
            version_format: "v{version}".into(),
            show_path: true,
            resize_message: "Dust — resize terminal".into(),
            content_width: 50,
            minimum_width: 20,
            minimum_height: 7,
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
    pub installed_plugins: BTreeSet<String>,
    pub user_configuration: bool,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub config: Config,
    pub path: PathBuf,
    pub diagnostic: Option<String>,
    /// A problem that doesn't stop the file from applying, such as a setting Dust doesn't have.
    pub warning: Option<String>,
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
