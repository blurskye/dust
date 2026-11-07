//! Configuration sources, consistent reads and background reloads.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use serde::Deserialize;
use thiserror::Error;

use crate::config::Config;
use crate::config::DashboardConfig;
use crate::config::KeyGroupEntry;
use crate::config::KeymapEntry;
use crate::config::LoadedConfig;
use crate::config::LoadedKeymaps;
use crate::config::migrate_config_source;
use crate::config::settings::ThemeConfig;
use crate::config::themes::ThemeRead;

pub fn user_state_directory() -> PathBuf {
    if let Some(root) = env::var_os("DUST_STATE_HOME") {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust")
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("XDG_STATE_HOME")
            .map(|root| PathBuf::from(root).join("dust"))
            .or_else(|| {
                env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support/Dust"))
            })
            .unwrap_or_else(|| PathBuf::from("dust"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dust")
    }
}

#[must_use]
pub fn user_config_path() -> PathBuf {
    if let Some(root) = env::var_os("DUST_CONFIG_HOME") {
        return PathBuf::from(root).join("config.toml");
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust")
            .join("config.toml")
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(|root| PathBuf::from(root).join("dust"))
            .or_else(|| {
                env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support/Dust"))
            })
            .unwrap_or_else(|| PathBuf::from("dust"))
            .join("config.toml")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dust/config.toml")
    }
}

#[must_use]
pub fn user_keymaps_path() -> PathBuf {
    user_config_path().with_file_name("keymaps.toml")
}

#[must_use]
pub fn user_dashboard_path() -> PathBuf {
    user_config_path().with_file_name("dashboard.toml")
}

#[must_use]
pub fn user_theme_path(name: &str) -> PathBuf {
    user_config_path()
        .with_file_name("themes")
        .join(format!("{name}.toml"))
}

pub(super) const BUNDLED_CONFIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/config.toml"
));
pub(super) const BUNDLED_KEYMAPS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/keymaps.toml"
));
pub(super) const BUNDLED_DASHBOARD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/defaults/dashboard.toml"
));
pub(super) const BUNDLED_THEME: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/themes/dust.toml"
));

/// The bundled defaults, parsed once. Startup and every reload need them several times, and
/// parsing about 170 KB of TOML is most of the startup time.
#[must_use]
pub fn load_bundled_config() -> LoadedConfig {
    static BUNDLED: OnceLock<LoadedConfig> = OnceLock::new();
    BUNDLED.get_or_init(parse_bundled_config).clone()
}

fn bundled_settings() -> Config {
    static SETTINGS: OnceLock<Config> = OnceLock::new();
    SETTINGS
        .get_or_init(|| {
            bundled_settings_value()
                .try_into::<Config>()
                .expect("bundled config must remain valid")
        })
        .clone()
}

pub(super) fn bundled_settings_value() -> toml::Value {
    let value = bundled_value().clone();
    #[cfg(windows)]
    {
        let mut value = value;
        let windows = toml::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/defaults/windows.toml"
        )))
        .expect("bundled Windows defaults");
        merge_toml(&mut value, windows);
        value
    }
    #[cfg(unix)]
    value
}

fn bundled_value() -> &'static toml::Value {
    static VALUE: OnceLock<toml::Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        toml::from_str::<toml::Value>(BUNDLED_CONFIG).expect("bundled config must remain valid")
    })
}

fn parse_bundled_config() -> LoadedConfig {
    let config = bundled_settings();
    config
        .validate()
        .expect("bundled config policy must be covered by tests");
    let mut keymaps = parse_keymaps(BUNDLED_KEYMAPS, PathBuf::from("<bundled:keymaps>"));
    keymaps.bundled_count = keymaps.entries.len();
    keymaps.bundled_group_count = keymaps.groups.len();
    keymaps.user_spans.clear();
    keymaps.user_group_spans.clear();
    let dashboard = toml::from_str::<DashboardConfig>(BUNDLED_DASHBOARD)
        .expect("bundled dashboard must be covered by tests");
    dashboard
        .validate()
        .expect("bundled dashboard dimensions must be covered by tests");
    let theme = bundled_theme();
    theme
        .validate()
        .expect("bundled theme fallbacks must be covered by tests");
    LoadedConfig {
        source_fingerprint: None,
        installed_plugins: BTreeSet::new(),
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config,
        path: PathBuf::from("<bundled:config>"),
        diagnostic: None,
        warning: None,
        keymaps,
        dashboard,
        dashboard_path: PathBuf::from("<bundled:dashboard>"),
        dashboard_spans: BTreeMap::new(),
        theme,
        theme_path: PathBuf::from("<bundled:theme:dust>"),
        theme_spans: BTreeMap::new(),
    }
}

#[derive(Clone, Debug)]
pub(super) enum CapturedText {
    Missing,
    Text(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub(super) struct CapturedSource {
    pub(super) path: PathBuf,
    pub(super) text: CapturedText,
    pub(super) identity: SourceFingerprint,
}

#[derive(Clone, Debug)]
pub(super) struct ConfigurationSourceBundle {
    pub(super) config: CapturedSource,
    pub(super) keymaps: CapturedSource,
    pub(super) dashboard: CapturedSource,
    pub(super) theme: CapturedSource,
    pub(super) selected_theme: String,
}

impl ConfigurationSourceBundle {
    pub(super) fn identity(&self) -> Vec<SourceFingerprint> {
        [&self.config, &self.keymaps, &self.dashboard, &self.theme]
            .into_iter()
            .map(|source| source.identity.clone())
            .collect()
    }

    pub(super) fn into_loaded(self) -> LoadedConfig {
        let bundled = load_bundled_config();
        let mut loaded = load_config_capture(&self.config);
        loaded.user_configuration = true;
        let user_keymaps = match &self.keymaps.text {
            CapturedText::Missing => LoadedKeymaps {
                path: self.keymaps.path.clone(),
                ..LoadedKeymaps::default()
            },
            CapturedText::Text(source) => parse_keymaps(source, self.keymaps.path.clone()),
            CapturedText::Error(error) => LoadedKeymaps {
                path: self.keymaps.path.clone(),
                diagnostic: Some(format!(
                    "cannot read {}: {error}",
                    self.keymaps.path.display()
                )),
                ..LoadedKeymaps::default()
            },
        };
        let (bundled_entries, bundled_groups) = (bundled.keymaps.entries, bundled.keymaps.groups);
        let bundled_count = bundled_entries.len();
        let bundled_group_count = bundled_groups.len();
        loaded.keymaps = LoadedKeymaps {
            entries: bundled_entries
                .into_iter()
                .chain(user_keymaps.entries)
                .collect(),
            groups: bundled_groups
                .into_iter()
                .chain(user_keymaps.groups)
                .collect(),
            bundled_count,
            bundled_group_count,
            path: user_keymaps.path,
            diagnostic: user_keymaps.diagnostic,
            user_spans: user_keymaps.user_spans,
            user_group_spans: user_keymaps.user_group_spans,
        };
        match parse_dashboard_capture(&self.dashboard) {
            Ok(Some(dashboard)) => {
                loaded.dashboard_spans = match &self.dashboard.text {
                    CapturedText::Text(source) => setting_spans(source),
                    CapturedText::Missing | CapturedText::Error(_) => BTreeMap::new(),
                };
                loaded.dashboard = dashboard;
                loaded.dashboard_path = self.dashboard.path.clone();
            }
            Ok(None) => {
                loaded.dashboard = bundled.dashboard.clone();
                loaded.dashboard_path = bundled.dashboard_path.clone();
            }
            Err(diagnostic) => {
                loaded.dashboard = bundled.dashboard.clone();
                loaded.dashboard_path = bundled.dashboard_path.clone();
                append_diagnostic(&mut loaded.diagnostic, diagnostic);
            }
        }
        if self.selected_theme == bundled.theme.name
            && matches!(self.theme.text, CapturedText::Missing)
        {
            loaded.theme = bundled.theme;
            loaded.theme_path = bundled.theme_path;
        } else if let Some(choice) = matches!(self.theme.text, CapturedText::Missing)
            .then(|| crate::config::themes::bundled_named(&self.selected_theme))
            .flatten()
        {
            loaded.theme = choice.theme;
            loaded.theme_path = choice.path;
        } else {
            match parse_theme_capture(&self.theme) {
                Ok(theme) if theme.name == self.selected_theme => {
                    loaded.theme_spans = match &self.theme.text {
                        CapturedText::Text(source) => setting_spans(source),
                        CapturedText::Missing | CapturedText::Error(_) => BTreeMap::new(),
                    };
                    loaded.theme = theme;
                    loaded.theme_path = self.theme.path.clone();
                }
                Ok(theme) => {
                    loaded.theme = bundled.theme;
                    loaded.theme_path = bundled.theme_path;
                    append_diagnostic(
                        &mut loaded.diagnostic,
                        format!(
                            "theme {} declares name `{}` instead of `{}`; using bundled theme",
                            self.theme.path.display(),
                            theme.name,
                            self.selected_theme
                        ),
                    );
                }
                Err(diagnostic) => {
                    loaded.theme = bundled.theme;
                    loaded.theme_path = bundled.theme_path;
                    append_diagnostic(&mut loaded.diagnostic, diagnostic);
                }
            }
        }
        loaded
    }
}

pub(super) fn capture_configuration_bundle_with(
    theme_hint: &str,
    mut capture: impl FnMut(PathBuf) -> CapturedSource,
) -> ConfigurationSourceBundle {
    let config = capture(user_config_path());
    let selected_theme = load_config_capture(&config).config.ui.theme;
    let keymaps = capture(user_keymaps_path());
    let dashboard = capture(user_dashboard_path());
    let theme_name = if selected_theme.is_empty() {
        theme_hint
    } else {
        &selected_theme
    };
    let theme = capture(user_theme_path(theme_name));
    ConfigurationSourceBundle {
        config,
        keymaps,
        dashboard,
        theme,
        selected_theme,
    }
}

pub(super) fn capture_configuration_bundle(theme_hint: &str) -> ConfigurationSourceBundle {
    capture_configuration_bundle_with(theme_hint, capture_source)
}

#[must_use]
pub fn load_user_config() -> LoadedConfig {
    match capture_consistent_configuration_bundle("dust") {
        Ok((bundle, fingerprint)) => {
            let mut loaded = bundle.into_loaded();
            loaded.source_fingerprint = Some(fingerprint);
            loaded
        }
        Err(diagnostic) => {
            let mut loaded = fallback_config(Some(diagnostic));
            loaded.user_configuration = true;
            loaded
        }
    }
}

pub(super) fn hash_source_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn capture_source(path: PathBuf) -> CapturedSource {
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CapturedSource {
                identity: SourceFingerprint {
                    path: path.clone(),
                    modified: None,
                    length: None,
                    content_hash: None,
                },
                path,
                text: CapturedText::Missing,
            };
        }
        Err(error) => {
            return CapturedSource {
                identity: SourceFingerprint {
                    path: path.clone(),
                    modified: None,
                    length: None,
                    content_hash: None,
                },
                path,
                text: CapturedText::Error(error.to_string()),
            };
        }
    };
    let modified = metadata.modified().ok();
    let length = metadata.len();
    let bytes = crate::files::open_regular_file(&path).and_then(|file| {
        let mut reader = file.take(WATCH_FINGERPRINT_BYTE_LIMIT.saturating_add(1));
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let (text, content_hash) = match bytes {
        Ok(bytes) => {
            let hash_length = bytes.len().min(WATCH_FINGERPRINT_BYTE_LIMIT as usize);
            let hash = Some(hash_source_bytes(&bytes[..hash_length]));
            if length > WATCH_FINGERPRINT_BYTE_LIMIT
                || bytes.len() as u64 > WATCH_FINGERPRINT_BYTE_LIMIT
            {
                (
                    CapturedText::Error(format!(
                        "configuration source exceeds {} byte limit",
                        WATCH_FINGERPRINT_BYTE_LIMIT
                    )),
                    hash,
                )
            } else {
                match String::from_utf8(bytes) {
                    Ok(source) => (CapturedText::Text(source), hash),
                    Err(error) => (CapturedText::Error(error.to_string()), hash),
                }
            }
        }
        Err(error) => (CapturedText::Error(error.to_string()), None),
    };
    CapturedSource {
        identity: SourceFingerprint {
            path: path.clone(),
            modified,
            length: Some(length),
            content_hash,
        },
        path,
        text,
    }
}

pub(super) fn read_configuration_source(path: &Path) -> std::io::Result<String> {
    match capture_source(path.to_path_buf()).text {
        CapturedText::Text(source) => Ok(source),
        CapturedText::Missing => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        CapturedText::Error(error) => {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }
    }
}

#[must_use]
pub fn load_keymaps(path: &Path) -> LoadedKeymaps {
    let source = match read_configuration_source(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedKeymaps {
                path: path.to_path_buf(),
                ..LoadedKeymaps::default()
            };
        }
        Err(error) => {
            return LoadedKeymaps {
                path: path.to_path_buf(),
                diagnostic: Some(format!("cannot read {}: {error}", path.display())),
                ..LoadedKeymaps::default()
            };
        }
    };
    parse_keymaps(&source, path.to_path_buf())
}

pub(crate) fn parse_keymaps(source: &str, path: PathBuf) -> LoadedKeymaps {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KeymapFile {
        #[serde(default)]
        keymap: Vec<toml::Spanned<KeymapEntry>>,
        #[serde(default)]
        key_group: Vec<toml::Spanned<KeyGroupEntry>>,
    }
    match toml::from_str::<KeymapFile>(source) {
        Ok(file) => {
            let (entries, user_spans) = expand_global_keymaps(file.keymap);
            let (groups, user_group_spans) = expand_global_groups(file.key_group);
            LoadedKeymaps {
                user_spans,
                user_group_spans,
                entries,
                groups,
                bundled_count: 0,
                bundled_group_count: 0,
                path,
                diagnostic: None,
            }
        }
        Err(error) => LoadedKeymaps {
            diagnostic: Some(format!(
                "invalid {}: {error}; ignoring keymaps",
                path.display()
            )),
            path,
            ..LoadedKeymaps::default()
        },
    }
}

pub(super) fn append_diagnostic(target: &mut Option<String>, diagnostic: String) {
    *target = Some(match target.take() {
        Some(existing) => format!("{existing}; {diagnostic}"),
        None => diagnostic,
    });
}

pub(super) fn bundled_theme() -> ThemeConfig {
    static THEME: OnceLock<ThemeConfig> = OnceLock::new();
    THEME
        .get_or_init(|| toml::from_str(BUNDLED_THEME).expect("bundled theme must remain valid"))
        .clone()
}

pub(super) fn parse_theme_capture(captured: &CapturedSource) -> Result<ThemeConfig, String> {
    let source = match &captured.text {
        CapturedText::Text(source) => source,
        CapturedText::Missing => {
            return Err(format!(
                "cannot read theme {}: not found",
                captured.path.display()
            ));
        }
        CapturedText::Error(error) => {
            return Err(format!(
                "cannot read theme {}: {error}",
                captured.path.display()
            ));
        }
    };
    let path = &captured.path;
    let theme = toml::from_str(source).map_err(|error| {
        format!(
            "invalid theme {}: {error}; using bundled theme",
            path.display()
        )
    })?;
    ThemeConfig::validate(&theme).map_err(|error| {
        format!(
            "invalid theme {}: {error}; using bundled theme",
            path.display()
        )
    })?;
    Ok(theme)
}

#[cfg(test)]
pub(super) fn load_theme(path: &Path) -> Result<ThemeConfig, String> {
    parse_theme_capture(&capture_source(path.to_path_buf()))
}

pub(super) fn parse_dashboard_capture(
    captured: &CapturedSource,
) -> Result<Option<DashboardConfig>, String> {
    let source = match &captured.text {
        CapturedText::Text(source) => source,
        CapturedText::Missing => return Ok(None),
        CapturedText::Error(error) => {
            return Err(format!("cannot read {}: {error}", captured.path.display()));
        }
    };
    let path = &captured.path;
    let dashboard: DashboardConfig = toml::from_str(source).map_err(|error| {
        format!(
            "invalid {}: {error}; using bundled dashboard",
            path.display()
        )
    })?;
    dashboard.validate().map_err(|error| {
        format!(
            "invalid {}: {error}; using bundled dashboard",
            path.display()
        )
    })?;
    Ok(Some(dashboard))
}

pub(super) fn fallback_config(diagnostic: Option<String>) -> LoadedConfig {
    LoadedConfig {
        source_fingerprint: None,
        installed_plugins: BTreeSet::new(),
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config: bundled_settings(),
        path: PathBuf::from("<bundled:config>"),
        diagnostic,
        warning: None,
        keymaps: LoadedKeymaps::default(),
        dashboard: DashboardConfig::default(),
        dashboard_path: PathBuf::from("<bundled:dashboard>"),
        dashboard_spans: BTreeMap::new(),
        theme: bundled_theme(),
        theme_path: PathBuf::from("<bundled:theme:dust>"),
        theme_spans: BTreeMap::new(),
    }
}

pub(super) fn load_config_capture(captured: &CapturedSource) -> LoadedConfig {
    match &captured.text {
        CapturedText::Missing => fallback_config(None),
        CapturedText::Error(error) => fallback_config(Some(format!(
            "cannot read {}: {error}",
            captured.path.display()
        ))),
        CapturedText::Text(source) => parse_config_source(&captured.path, source),
    }
}

#[must_use]
pub fn load_config(path: &Path) -> LoadedConfig {
    load_config_capture(&capture_source(path.to_path_buf()))
}

pub(super) fn parse_config_source(path: &Path, source: &str) -> LoadedConfig {
    match settings_from_source(source) {
        Ok((config, user_setting_keys, _, ignored)) => LoadedConfig {
            source_fingerprint: None,
            installed_plugins: BTreeSet::new(),
            user_configuration: false,
            user_setting_keys,
            user_setting_spans: setting_spans(source),
            config,
            path: path.to_path_buf(),
            diagnostic: None,
            warning: (!ignored.is_empty()).then(|| {
                let file = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                format!(
                    "left out settings Dust doesn't have from {file}: {}",
                    ignored.join(", ")
                )
            }),
            keymaps: LoadedKeymaps::default(),
            dashboard: DashboardConfig::default(),
            dashboard_path: PathBuf::from("<bundled:dashboard>"),
            dashboard_spans: BTreeMap::new(),
            theme: bundled_theme(),
            theme_path: PathBuf::from("<bundled:theme:dust>"),
            theme_spans: BTreeMap::new(),
        },
        // What happens next is the caller's to say: startup uses the bundled settings, a reload
        // keeps the ones in use.
        Err(error) => fallback_config(Some(format!("invalid {}: {error}", path.display()))),
    }
}

/// The settings a user config.toml gives over the bundled defaults, with the keys it sets, its
/// source after migration and the keys it has that Dust doesn't, which are left out so an old
/// or misspelt setting doesn't cost the rest of the file; an error when it does not parse or
/// holds an invalid value.
pub(super) fn settings_from_source(
    source: &str,
) -> Result<(Config, BTreeSet<String>, String, Vec<String>), String> {
    let (source, _) = migrate_config_source(source)?;
    let mut user = toml::from_str::<toml::Value>(&source).map_err(|error| error.to_string())?;
    let mut ignored = Vec::new();
    let config = loop {
        let mut merged = bundled_settings_value();
        merge_toml(&mut merged, user.clone());
        let message = match merged.try_into::<Config>() {
            Ok(config) => break config,
            Err(error) => error.to_string(),
        };
        match unknown_field(&message) {
            Some(key) if ignored.len() < 64 && remove_key(&mut user, &key) => {
                ignored.push(key.join("."))
            }
            _ => return Err(message),
        }
    };
    config.validate()?;
    Ok((config, setting_keys(&user), source, ignored))
}

/// The dotted key of the field serde refused in "unknown field `name`, expected ...\nin `a.b`".
fn unknown_field(message: &str) -> Option<Vec<String>> {
    let (_, rest) = message.split_once("unknown field `")?;
    let (name, _) = rest.split_once('`')?;
    let mut key: Vec<String> = message
        .lines()
        .find_map(|line| line.strip_prefix("in "))
        .map(|table| {
            table
                .trim()
                .trim_matches('`')
                .split('.')
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    key.push(name.to_owned());
    Some(key)
}

fn remove_key(value: &mut toml::Value, key: &[String]) -> bool {
    if let Some(array) = value.as_array_mut() {
        if let Some(index) = key.first().and_then(|index| index.parse::<usize>().ok()) {
            return array
                .get_mut(index)
                .is_some_and(|value| remove_key(value, &key[1..]));
        }
        // TOML's value deserializer omits array indices. Window rules are the only
        // configuration array of tables; locate the rule that actually rejects this
        // field, rather than deleting a valid field from an earlier placement variant.
        return array.iter_mut().any(|value| {
            let rejected = value
                .clone()
                .try_into::<crate::config::WindowRuleConfig>()
                .err()
                .and_then(|error| unknown_field(&error.to_string()));
            rejected.as_deref() == Some(key) && remove_key(value, key)
        });
    }
    let Some(table) = value.as_table_mut() else {
        return false;
    };
    if key.len() == 1 {
        return table.remove(&key[0]).is_some();
    }
    // Serde prints table names with dots without quoting literal dots in keys.
    // Match that display against the actual table keys, retaining their boundaries.
    for count in 1..key.len() {
        let parent = key[..count].join(".");
        if table
            .get_mut(&parent)
            .is_some_and(|value| remove_key(value, &key[count..]))
        {
            return true;
        }
    }
    false
}

/// A setting's bundled default value, by its dotted key.
pub(super) fn bundled_setting(key: &str) -> Option<toml::Value> {
    let defaults = bundled_settings_value();
    let mut value = &defaults;
    for segment in crate::config::config_key_segments(key)? {
        value = value.get(&segment)?;
    }
    Some(value.clone())
}

pub(crate) fn setting_spans(source: &str) -> BTreeMap<String, std::ops::Range<usize>> {
    fn visit(
        table: &dyn toml_edit::TableLike,
        prefix: &str,
        spans: &mut BTreeMap<String, std::ops::Range<usize>>,
    ) {
        for (key, item) in table.iter() {
            let path = if prefix.is_empty() {
                key.into()
            } else {
                format!("{prefix}.{key}")
            };
            if let Some(nested) = item.as_table_like() {
                visit(nested, &path, spans);
            } else if let Some(array) = item.as_array_of_tables() {
                for (index, table) in array.iter().enumerate() {
                    visit(table, &format!("{path}.{index}"), spans);
                }
            } else if let Some(span) = item.span() {
                spans.insert(path, span);
            }
        }
    }

    let Ok(document) = source.parse::<toml_edit::Document<String>>() else {
        return BTreeMap::new();
    };
    let mut spans = BTreeMap::new();
    visit(document.as_table(), "", &mut spans);
    spans
}

pub(super) fn setting_keys(value: &toml::Value) -> BTreeSet<String> {
    fn visit(value: &toml::Value, prefix: &str, output: &mut BTreeSet<String>) {
        if let Some(table) = value.as_table() {
            for (key, value) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if value.is_table() {
                    visit(value, &path, output);
                } else {
                    output.insert(path);
                }
            }
        }
    }
    let mut output = BTreeSet::new();
    visit(value, "", &mut output);
    output
}

pub(super) fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_toml(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

pub struct ConfigurationWorker {
    sender: SyncSender<ConfigurationRequest>,
    receiver: Receiver<ConfigurationWorkerResult>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct ConfigurationWorkerResult {
    pub reload_id: u64,
    pub automatic: bool,
    pub candidate: Result<LoadedConfig, String>,
}

pub(super) enum ConfigurationRequest {
    Reload,
    Themes(ThemeRead),
    VerifyWatch,
    Shutdown,
}

/// How often the user's configuration files are checked for changes, and how long they must
/// stay unchanged before they are reloaded.
const CONFIG_WATCH_POLL_MS: u64 = 1000;
const CONFIG_WATCH_DEBOUNCE_MS: u64 = 100;

impl ConfigurationWorker {
    #[must_use]
    pub fn start(use_user_configuration: bool, theme: String) -> Self {
        Self::start_with_fingerprint(use_user_configuration, theme, None)
    }

    pub(crate) fn start_with_fingerprint(
        use_user_configuration: bool,
        mut theme: String,
        initial_fingerprint: Option<Vec<SourceFingerprint>>,
    ) -> Self {
        let (sender, requests) = mpsc::sync_channel(4);
        let (results, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-configuration".into())
            .spawn(move || {
                let mut reload_id = 0_u64;
                // Without a loaded snapshot, verify startup with a reload. Otherwise
                // compare against what was loaded so unchanged startup doesn't reset
                // view toggles or cancel work the user has already started.
                let mut changed_at =
                    (use_user_configuration && initial_fingerprint.is_none()).then(Instant::now);
                let mut fingerprint = if use_user_configuration {
                    initial_fingerprint.unwrap_or_else(|| configuration_fingerprint(&theme))
                } else {
                    Vec::new()
                };
                loop {
                    let request = if use_user_configuration {
                        requests.recv_timeout(Duration::from_millis(CONFIG_WATCH_POLL_MS))
                    } else {
                        requests.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match request {
                        Ok(ConfigurationRequest::Themes(request)) => {
                            crate::config::themes::discover(request)
                        }
                        Ok(ConfigurationRequest::Reload) => {
                            reload_id = reload_id.saturating_add(1);
                            let candidate =
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        theme = consistent.theme;
                                        if let Some(stable) = consistent.fingerprint {
                                            fingerprint = stable;
                                        }
                                        changed_at = None;
                                        consistent.candidate
                                    }
                                    Err(diagnostic) => {
                                        if use_user_configuration {
                                            fingerprint = configuration_fingerprint(&theme);
                                            changed_at = Some(Instant::now());
                                        }
                                        Err(diagnostic)
                                    }
                                };
                            if results
                                .send(ConfigurationWorkerResult {
                                    reload_id,
                                    automatic: false,
                                    candidate,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(ConfigurationRequest::VerifyWatch) => {
                            if !use_user_configuration {
                                continue;
                            }
                            // The worker watches the captured on-disk theme, even if the UI
                            // rejected it. An acknowledgment must not erase a pending edit.
                            let observed = configuration_fingerprint(&theme);
                            if observed != fingerprint {
                                fingerprint = observed;
                                changed_at = Some(Instant::now());
                            }
                        }
                        Ok(ConfigurationRequest::Shutdown)
                        | Err(RecvTimeoutError::Disconnected) => {
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            let observed = configuration_fingerprint(&theme);
                            if observed != fingerprint {
                                fingerprint = observed;
                                changed_at = Some(Instant::now());
                            }
                            if changed_at.is_some_and(|changed| {
                                changed.elapsed() >= Duration::from_millis(CONFIG_WATCH_DEBOUNCE_MS)
                            }) {
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        theme = consistent.theme;
                                        if let Some(stable) = consistent.fingerprint {
                                            fingerprint = stable;
                                        }
                                        changed_at = None;
                                        reload_id = reload_id.saturating_add(1);
                                        if results
                                            .send(ConfigurationWorkerResult {
                                                reload_id,
                                                automatic: true,
                                                candidate: consistent.candidate,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        fingerprint = configuration_fingerprint(&theme);
                                        changed_at = Some(Instant::now());
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .expect("configuration worker thread must start");
        Self {
            sender,
            receiver,
            thread: Some(thread),
        }
    }

    pub(crate) fn request_themes(
        &self,
        request: ThemeRead,
    ) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::Themes(request))
            .map_err(map_configuration_send_error)
    }

    pub fn request_reload(&self) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::Reload)
            .map_err(map_configuration_send_error)
    }

    pub fn verify_watch(&self) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::VerifyWatch)
            .map_err(map_configuration_send_error)
    }

    pub fn try_result(
        &self,
    ) -> Result<Option<ConfigurationWorkerResult>, ConfigurationWorkerError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ConfigurationWorkerError::Stopped),
        }
    }
}

pub(super) fn map_configuration_send_error(
    error: TrySendError<ConfigurationRequest>,
) -> ConfigurationWorkerError {
    match error {
        TrySendError::Full(_) => ConfigurationWorkerError::Busy,
        TrySendError::Disconnected(_) => ConfigurationWorkerError::Stopped,
    }
}

pub(super) struct CandidateAttempt {
    pub(super) candidate: Result<LoadedConfig, String>,
    pub(super) theme: String,
}

pub(super) fn candidate_attempt_from_loaded(loaded: LoadedConfig) -> CandidateAttempt {
    let theme = loaded.config.ui.theme.clone();
    let mut diagnostics = Vec::new();
    diagnostics.extend(loaded.diagnostic.iter().cloned());
    diagnostics.extend(loaded.keymaps.diagnostic.iter().cloned());
    CandidateAttempt {
        candidate: if diagnostics.is_empty() {
            Ok(loaded)
        } else {
            Err(diagnostics.join("; "))
        },
        theme,
    }
}

pub(super) struct ConsistentCandidate<F> {
    pub(super) candidate: Result<LoadedConfig, String>,
    pub(super) theme: String,
    pub(super) fingerprint: Option<F>,
}

pub(super) const CONFIGURATION_CONSISTENCY_ATTEMPTS: usize = 3;

pub(super) fn captured_bundle_is_stable<F: Eq>(before: &F, captured: &F, after: &F) -> bool {
    before == captured && captured == after
}

pub(super) fn capture_consistent_configuration_bundle(
    initial_theme: &str,
) -> Result<(ConfigurationSourceBundle, Vec<SourceFingerprint>), String> {
    capture_consistent_configuration_bundle_with(
        initial_theme,
        capture_configuration_bundle,
        configuration_fingerprint,
    )
}

pub(super) fn capture_consistent_configuration_bundle_with(
    initial_theme: &str,
    mut capture: impl FnMut(&str) -> ConfigurationSourceBundle,
    mut fingerprint: impl FnMut(&str) -> Vec<SourceFingerprint>,
) -> Result<(ConfigurationSourceBundle, Vec<SourceFingerprint>), String> {
    let mut theme = initial_theme.to_owned();
    for _ in 0..CONFIGURATION_CONSISTENCY_ATTEMPTS {
        let before = fingerprint(&theme);
        let bundle = capture(&theme);
        if bundle.selected_theme != theme {
            theme = bundle.selected_theme.clone();
            continue;
        }
        let captured = bundle.identity();
        let after = fingerprint(&theme);
        if captured_bundle_is_stable(&before, &captured, &after) {
            return Ok((bundle, captured));
        }
    }
    Err("configuration sources changed while a complete candidate was loading; no snapshot was applied"
        .into())
}

pub(super) fn load_consistent_configuration(
    use_user_configuration: bool,
    initial_theme: &str,
) -> Result<ConsistentCandidate<Vec<SourceFingerprint>>, String> {
    if !use_user_configuration {
        let attempt = candidate_attempt_from_loaded(load_bundled_config());
        return Ok(ConsistentCandidate {
            candidate: attempt.candidate,
            theme: attempt.theme,
            fingerprint: None,
        });
    }
    let (bundle, captured) = capture_consistent_configuration_bundle(initial_theme)?;
    let theme = bundle.selected_theme.clone();
    let attempt = candidate_attempt_from_loaded(bundle.into_loaded());
    Ok(ConsistentCandidate {
        candidate: attempt.candidate,
        theme,
        fingerprint: Some(captured),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceFingerprint {
    pub(super) path: PathBuf,
    pub(super) modified: Option<SystemTime>,
    pub(super) length: Option<u64>,
    /// Hashes at most this process-owned limit, so polling cannot allocate or
    /// read an unbounded file merely to detect a same-metadata replacement.
    pub(super) content_hash: Option<u64>,
}

pub(super) const WATCH_FINGERPRINT_BYTE_LIMIT: u64 = 4 * 1024 * 1024;

pub(super) fn source_fingerprint(path: PathBuf) -> SourceFingerprint {
    let metadata = fs::metadata(&path).ok();
    let content_hash = crate::files::open_regular_file(&path)
        .ok()
        .and_then(|file| {
            let mut reader = file.take(WATCH_FINGERPRINT_BYTE_LIMIT);
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).ok()?;
            Some(hash_source_bytes(&bytes))
        });
    SourceFingerprint {
        path,
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        length: metadata.map(|metadata| metadata.len()),
        content_hash,
    }
}

pub(super) fn configuration_fingerprint(theme: &str) -> Vec<SourceFingerprint> {
    [
        user_config_path(),
        user_keymaps_path(),
        user_dashboard_path(),
        user_theme_path(theme),
    ]
    .into_iter()
    .map(source_fingerprint)
    .collect()
}

impl Drop for ConfigurationWorker {
    fn drop(&mut self) {
        let _ = self.sender.try_send(ConfigurationRequest::Shutdown);
        // Configuration filesystem I/O must not add a blocking shutdown join.
        // Dropping the sender also stops the worker after accepted requests finish.
        drop(self.thread.take());
    }
}

#[cfg(test)]
mod shutdown_tests {
    use crate::config::load::*;
    #[test]
    fn dropping_configuration_worker_does_not_wait_for_a_full_queue_or_blocked_worker() {
        let (sender, requests) = mpsc::sync_channel(4);
        let (_results, receiver) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let thread = thread::spawn(move || {
            gate.recv().unwrap();
            while let Ok(request) = requests.recv() {
                if matches!(request, ConfigurationRequest::Shutdown) {
                    break;
                }
            }
        });
        for _ in 0..4 {
            assert!(sender.try_send(ConfigurationRequest::Reload).is_ok());
        }
        let worker = ConfigurationWorker {
            sender,
            receiver,
            thread: Some(thread),
        };
        let (finished, completion) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(worker);
            finished.send(()).unwrap();
        });
        let nonblocking = completion.recv_timeout(Duration::from_secs(1)).is_ok();
        release.send(()).unwrap();
        dropper.join().unwrap();
        assert!(
            nonblocking,
            "drop waited for a full queue or a gated configuration worker"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConfigurationWorkerError {
    #[error("configuration reload is already pending")]
    Busy,
    #[error("configuration worker stopped")]
    Stopped,
}

/// Surfaces that receive `context = "global"` bindings: every place where keys are
/// commands rather than typed text, so the leader and window keys work everywhere.
const GLOBAL_KEYMAP_TARGETS: &[(&str, &str)] = &[
    ("editor", "normal"),
    ("explorer", ""),
    ("dashboard", ""),
    ("terminal_normal", ""),
    ("tool_manager", ""),
    ("panel", ""),
    ("agent_normal", ""),
];

fn normalized_keys(keys: &Option<String>) -> Option<String> {
    keys.as_ref().map(|keys| {
        keys.split_whitespace()
            .map(|token| {
                token
                    .parse::<crate::editor::input::KeyEvent>()
                    .map_or_else(|_| token.to_owned(), |key| key.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// A user entry that names a bundled global binding by id alone (to rebind or disable it)
/// applies to every surface copy of that binding.
fn names_global_identity(id: &str, context: &Option<String>) -> bool {
    if context.is_some() || !id.starts_with("builtin.global.") {
        return false;
    }
    static IDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        let bundled: toml::Value = toml::from_str(BUNDLED_KEYMAPS).expect("bundled keymaps");
        ["keymap", "key_group"]
            .into_iter()
            .filter_map(|kind| bundled.get(kind).and_then(toml::Value::as_array))
            .flatten()
            .filter(|entry| entry.get("context").and_then(toml::Value::as_str) == Some("global"))
            .filter_map(|entry| entry.get("id").and_then(toml::Value::as_str))
            .map(str::to_owned)
            .collect()
    })
    .contains(id)
}

fn global_target_id(id: &str, context: &str, mode: &str) -> String {
    if mode.is_empty() {
        format!("{id}.{}", context.replace('_', "-"))
    } else {
        format!("{id}.{context}-{}", mode.replace('_', "-"))
    }
}

/// Copies each `global` keymap into every target surface, unless that surface declares
/// the same keys itself in this file.
fn expand_global_keymaps(
    keymaps: Vec<toml::Spanned<KeymapEntry>>,
) -> (Vec<KeymapEntry>, Vec<std::ops::Range<usize>>) {
    let declared = keymaps
        .iter()
        .map(|entry| entry.get_ref())
        .filter(|entry| entry.context.as_deref() != Some("global"))
        .filter_map(|entry| {
            Some((
                entry.context.clone()?,
                entry.mode.clone(),
                normalized_keys(&entry.keys)?,
            ))
        })
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::with_capacity(keymaps.len());
    let mut spans = Vec::with_capacity(keymaps.len());
    for spanned in keymaps {
        let span = spanned.span();
        let entry = spanned.into_inner();
        if names_global_identity(&entry.id, &entry.context) {
            for (context, mode) in GLOBAL_KEYMAP_TARGETS {
                entries.push(KeymapEntry {
                    id: global_target_id(&entry.id, context, mode),
                    ..entry.clone()
                });
                spans.push(span.clone());
            }
            continue;
        }
        if entry.context.as_deref() != Some("global") {
            entries.push(entry);
            spans.push(span);
            continue;
        }
        let keys = normalized_keys(&entry.keys);
        for (context, mode) in GLOBAL_KEYMAP_TARGETS {
            if keys.as_ref().is_some_and(|keys| {
                declared.contains(&((*context).into(), (*mode).into(), keys.clone()))
            }) {
                continue;
            }
            entries.push(KeymapEntry {
                id: global_target_id(&entry.id, context, mode),
                context: Some((*context).into()),
                mode: (*mode).into(),
                ..entry.clone()
            });
            spans.push(span.clone());
        }
    }
    (entries, spans)
}

fn expand_global_groups(
    groups: Vec<toml::Spanned<KeyGroupEntry>>,
) -> (Vec<KeyGroupEntry>, Vec<std::ops::Range<usize>>) {
    let mut entries = Vec::with_capacity(groups.len());
    let mut spans = Vec::with_capacity(groups.len());
    for spanned in groups {
        let span = spanned.span();
        let group = spanned.into_inner();
        if names_global_identity(&group.id, &group.context) {
            for (context, mode) in GLOBAL_KEYMAP_TARGETS {
                entries.push(KeyGroupEntry {
                    id: global_target_id(&group.id, context, mode),
                    ..group.clone()
                });
                spans.push(span.clone());
            }
            continue;
        }
        if group.context.as_deref() != Some("global") {
            entries.push(group);
            spans.push(span);
            continue;
        }
        for (context, mode) in GLOBAL_KEYMAP_TARGETS {
            entries.push(KeyGroupEntry {
                id: global_target_id(&group.id, context, mode),
                context: Some((*context).into()),
                mode: (*mode).into(),
                ..group.clone()
            });
            spans.push(span.clone());
        }
    }
    (entries, spans)
}
