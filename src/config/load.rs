use super::*;

pub fn user_state_directory() -> PathBuf {
    if let Some(root) = env::var_os("DUST_STATE_HOME") {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "windows")]
    {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust");
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/Dust");
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
        return env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust")
            .join("config.toml");
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/Dust/config.toml");
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

pub(super) const BUNDLED_CONFIG: &str = include_str!("../../assets/defaults/config.toml");
pub(super) const BUNDLED_KEYMAPS: &str = include_str!("../../assets/defaults/keymaps.toml");
pub(super) const BUNDLED_DASHBOARD: &str = include_str!("../../assets/defaults/dashboard.toml");
pub(super) const BUNDLED_THEME: &str = include_str!("../../assets/themes/dust.toml");

#[must_use]
pub fn load_bundled_config() -> LoadedConfig {
    let config =
        toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must be covered by tests");
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
    let theme = toml::from_str::<ThemeConfig>(BUNDLED_THEME)
        .expect("bundled theme must be covered by tests");
    theme
        .validate()
        .expect("bundled theme fallbacks must be covered by tests");
    LoadedConfig {
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config,
        path: PathBuf::from("<bundled:config>"),
        diagnostic: None,
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
        let (bundled_entries, bundled_groups) = if loaded.config.input.keymaps.load_bundled_defaults
        {
            (bundled.keymaps.entries, bundled.keymaps.groups)
        } else {
            (Vec::new(), Vec::new())
        };
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
        if self.selected_theme == bundled.theme.name {
            loaded.theme = bundled.theme;
            loaded.theme_path = bundled.theme_path;
        } else if let Some(choice) = matches!(self.theme.text, CapturedText::Missing)
            .then(|| super::themes::bundled_named(&self.selected_theme))
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
        Ok((bundle, _)) => bundle.into_loaded(),
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
    let bytes = fs::File::open(&path).and_then(|file| {
        let mut reader = file.take(WATCH_FINGERPRINT_BYTE_CEILING.saturating_add(1));
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let (text, content_hash) = match bytes {
        Ok(bytes) => {
            let hash_length = bytes.len().min(WATCH_FINGERPRINT_BYTE_CEILING as usize);
            let hash = Some(hash_source_bytes(&bytes[..hash_length]));
            if length > WATCH_FINGERPRINT_BYTE_CEILING
                || bytes.len() as u64 > WATCH_FINGERPRINT_BYTE_CEILING
            {
                (
                    CapturedText::Error(format!(
                        "configuration source exceeds {} byte ceiling",
                        WATCH_FINGERPRINT_BYTE_CEILING
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

pub(super) fn parse_keymaps(source: &str, path: PathBuf) -> LoadedKeymaps {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KeymapFile {
        #[serde(default)]
        keymap: Vec<toml::Spanned<KeymapEntry>>,
        #[serde(default)]
        key_group: Vec<toml::Spanned<KeyGroupEntry>>,
    }
    match toml::from_str::<KeymapFile>(source) {
        Ok(file) => LoadedKeymaps {
            user_spans: file.keymap.iter().map(toml::Spanned::span).collect(),
            user_group_spans: file.key_group.iter().map(toml::Spanned::span).collect(),
            entries: file
                .keymap
                .into_iter()
                .map(toml::Spanned::into_inner)
                .collect(),
            groups: file
                .key_group
                .into_iter()
                .map(toml::Spanned::into_inner)
                .collect(),
            bundled_count: 0,
            bundled_group_count: 0,
            path,
            diagnostic: None,
        },
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
    toml::from_str(BUNDLED_THEME).expect("bundled theme must remain valid")
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
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config: toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must remain valid"),
        path: PathBuf::from("<bundled:config>"),
        diagnostic,
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
    let migrated = match migrate_config_source(source) {
        Ok((source, _)) => source,
        Err(error) => {
            return fallback_config(Some(format!(
                "invalid {}: {error}; using bundled defaults",
                path.display()
            )));
        }
    };
    let source = migrated.as_str();
    let defaults =
        toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must remain valid");
    let mut merged =
        toml::from_str::<toml::Value>(BUNDLED_CONFIG).expect("bundled config must remain valid");
    let user = match toml::from_str::<toml::Value>(source) {
        Ok(user) => user,
        Err(error) => {
            return LoadedConfig {
                user_configuration: false,
                user_setting_keys: BTreeSet::new(),
                user_setting_spans: BTreeMap::new(),
                config: defaults,
                path: PathBuf::from("<bundled:config>"),
                diagnostic: Some(format!(
                    "invalid {}: {error}; using bundled defaults",
                    path.display()
                )),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            };
        }
    };
    let user_setting_keys = setting_keys(&user);
    merge_toml(&mut merged, user);
    match merged.try_into::<Config>() {
        Ok(config) => match config.validate() {
            Ok(()) => LoadedConfig {
                user_configuration: false,
                user_setting_keys,
                user_setting_spans: setting_spans(source),
                config,
                path: path.to_path_buf(),
                diagnostic: unknown_key_warning(source),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            },
            Err(error) => LoadedConfig {
                user_configuration: false,
                user_setting_keys: BTreeSet::new(),
                user_setting_spans: BTreeMap::new(),
                config: defaults,
                path: PathBuf::from("<bundled:config>"),
                diagnostic: Some(format!(
                    "invalid {}: {error}; using defaults",
                    path.display()
                )),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            },
        },
        Err(error) => LoadedConfig {
            user_configuration: false,
            user_setting_keys: BTreeSet::new(),
            user_setting_spans: BTreeMap::new(),
            config: defaults,
            path: PathBuf::from("<bundled:config>"),
            diagnostic: Some(format!(
                "invalid {}: {error}; using bundled defaults",
                path.display()
            )),
            keymaps: LoadedKeymaps::default(),
            dashboard: DashboardConfig::default(),
            dashboard_path: PathBuf::from("<bundled:dashboard>"),
            dashboard_spans: BTreeMap::new(),
            theme: bundled_theme(),
            theme_path: PathBuf::from("<bundled:theme:dust>"),
            theme_spans: BTreeMap::new(),
        },
    }
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
    pub generation: u64,
    pub automatic: bool,
    pub candidate: Result<LoadedConfig, String>,
}

pub(super) enum ConfigurationRequest {
    Reload,
    Themes(ThemeRead),
    ConfigureWatch {
        watch: ConfigWatchConfig,
        theme: String,
    },
    Shutdown,
}

impl ConfigurationWorker {
    #[must_use]
    pub fn start(
        use_user_configuration: bool,
        mut watch: ConfigWatchConfig,
        mut theme: String,
    ) -> Self {
        if !use_user_configuration {
            watch.enabled = false;
        }
        let (sender, requests) = mpsc::sync_channel(4);
        let (results, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-configuration".into())
            .spawn(move || {
                let mut generation = 0_u64;
                let mut fingerprint = if use_user_configuration {
                    configuration_fingerprint(&theme)
                } else {
                    Vec::new()
                };
                let mut changed_at: Option<Instant> = None;
                loop {
                    let request = if watch.enabled {
                        requests.recv_timeout(Duration::from_millis(watch.poll_interval_ms))
                    } else {
                        requests.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match request {
                        Ok(ConfigurationRequest::Themes(request)) => {
                            super::themes::discover(request)
                        }
                        Ok(ConfigurationRequest::Reload) => {
                            generation = generation.saturating_add(1);
                            let candidate =
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        if consistent.theme == theme {
                                            if let Some(stable) = consistent.fingerprint {
                                                fingerprint = stable;
                                            }
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
                                    generation,
                                    automatic: false,
                                    candidate,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(ConfigurationRequest::ConfigureWatch {
                            watch: configured,
                            theme: configured_theme,
                        }) => {
                            watch = configured;
                            if !use_user_configuration {
                                watch.enabled = false;
                                theme = configured_theme;
                                fingerprint.clear();
                                changed_at = None;
                                continue;
                            }
                            let theme_changed = theme != configured_theme;
                            theme = configured_theme;
                            let observed = configuration_fingerprint(&theme);
                            let verify_candidate = theme_changed || observed != fingerprint;
                            fingerprint = observed;
                            changed_at = verify_candidate.then(Instant::now);
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
                                changed.elapsed() >= Duration::from_millis(watch.debounce_ms)
                            }) {
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        if consistent.theme == theme {
                                            if let Some(stable) = consistent.fingerprint {
                                                fingerprint = stable;
                                            }
                                        }
                                        changed_at = None;
                                        generation = generation.saturating_add(1);
                                        if results
                                            .send(ConfigurationWorkerResult {
                                                generation,
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

    pub fn configure_watch(
        &self,
        watch: ConfigWatchConfig,
        theme: String,
    ) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::ConfigureWatch { watch, theme })
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

#[cfg(test)]
pub(super) fn load_consistent_candidate_with<F, L, P>(
    use_user_configuration: bool,
    initial_theme: &str,
    mut load: L,
    mut fingerprint: P,
) -> Result<ConsistentCandidate<F>, String>
where
    F: Eq,
    L: FnMut() -> CandidateAttempt,
    P: FnMut(&str) -> F,
{
    if !use_user_configuration {
        let attempt = load();
        return Ok(ConsistentCandidate {
            candidate: attempt.candidate,
            theme: attempt.theme,
            fingerprint: None,
        });
    }
    let mut theme = initial_theme.to_owned();
    for _ in 0..CONFIGURATION_CONSISTENCY_ATTEMPTS {
        let before = fingerprint(&theme);
        let attempt = load();
        if attempt.theme != theme {
            theme = attempt.theme;
            continue;
        }
        let after = fingerprint(&theme);
        if before == after {
            return Ok(ConsistentCandidate {
                candidate: attempt.candidate,
                theme,
                fingerprint: Some(after),
            });
        }
    }
    Err("configuration sources changed while a complete candidate was loading; no snapshot was applied"
        .into())
}

pub(super) fn captured_bundle_is_stable<F: Eq>(before: &F, captured: &F, after: &F) -> bool {
    before == captured && captured == after
}

pub(super) fn capture_consistent_configuration_bundle(
    initial_theme: &str,
) -> Result<(ConfigurationSourceBundle, Vec<SourceFingerprint>), String> {
    let mut theme = initial_theme.to_owned();
    for _ in 0..CONFIGURATION_CONSISTENCY_ATTEMPTS {
        let before = configuration_fingerprint(&theme);
        let bundle = capture_configuration_bundle(&theme);
        if bundle.selected_theme != theme {
            theme = bundle.selected_theme.clone();
            continue;
        }
        let captured = bundle.identity();
        let after = configuration_fingerprint(&theme);
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
pub(super) struct SourceFingerprint {
    pub(super) path: PathBuf,
    pub(super) modified: Option<SystemTime>,
    pub(super) length: Option<u64>,
    /// Hashes at most this process-owned ceiling, so polling cannot allocate or
    /// read an unbounded file merely to detect a same-metadata replacement.
    pub(super) content_hash: Option<u64>,
}

pub(super) const WATCH_FINGERPRINT_BYTE_CEILING: u64 = 4 * 1024 * 1024;

pub(super) fn source_fingerprint(path: PathBuf) -> SourceFingerprint {
    let metadata = fs::metadata(&path).ok();
    let content_hash = fs::File::open(&path).ok().and_then(|file| {
        let mut reader = file.take(WATCH_FINGERPRINT_BYTE_CEILING);
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
        // Dropping the sender also stops the worker after admitted requests finish.
        drop(self.thread.take());
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
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
        let (retired, retirement) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(worker);
            retired.send(()).unwrap();
        });
        let nonblocking = retirement.recv_timeout(Duration::from_secs(1)).is_ok();
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

pub(super) fn unknown_key_warning(source: &str) -> Option<String> {
    let value = toml::from_str::<toml::Value>(source).ok()?;
    let table = value.as_table()?;
    let unknown: Vec<_> = table
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "schema_version"
                    | "editor"
                    | "ui"
                    | "startup"
                    | "input"
                    | "search"
                    | "history"
                    | "recovery"
                    | "config_watch"
                    | "runtime"
                    | "lsp"
                    | "observability"
                    | "persistence"
                    | "processes"
                    | "packages"
                    | "plugins"
                    | "debug"
                    | "tasks"
                    | "syntax"
                    | "terminals"
                    | "windows"
                    | "workspace"
                    | "messages"
                    | "command_labels"
            )
        })
        .cloned()
        .collect();
    (!unknown.is_empty()).then(|| format!("unknown configuration sections: {}", unknown.join(", ")))
}
