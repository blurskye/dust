//! Configuration resolution, source locations and value explanations.

use crate::app::commands::AppEffect;
use crate::app::commands::register_app_commands;
use crate::app::configuration::KeymapCompilation;
use crate::app::configuration::compile::expanded_key_sequence;
use crate::app::configuration::compile_keymap_entries;
use crate::app::configuration::compile_keymaps;
use crate::app::configuration::configured_context;
use crate::app::configuration::keymap_entries;
use crate::app::configuration::literal_key_sequence;
use crate::app::configuration::plugin_owner_from_id;
use crate::app::configuration::validate_configured_commands;
use crate::config::LoadedConfig;
use crate::editor::CommandRegistry;
use crate::editor::input::InputContexts;
use crate::editor::register_builtin_commands;
use crate::ui::DashboardAction;
use std::collections::BTreeSet;
use std::path::PathBuf;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn enabled_plugin_owners(loaded: &LoadedConfig) -> BTreeSet<String> {
    let mut enabled = loaded
        .config
        .plugins
        .entries
        .iter()
        .filter(|(_, entry)| entry.enabled)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    enabled.extend(
        loaded
            .installed_plugins
            .iter()
            .filter(|owner| {
                loaded
                    .config
                    .plugins
                    .entries
                    .get(*owner)
                    .is_none_or(|entry| entry.enabled)
            })
            .cloned(),
    );
    enabled
}

pub(in crate::app) fn base_safe_keymaps(
    loaded: &LoadedConfig,
) -> (crate::config::LoadedKeymaps, BTreeSet<String>, bool) {
    let enabled = enabled_plugin_owners(loaded);
    let mut base = loaded.keymaps.clone();
    let mut entries = base.entries[..base.bundled_count].to_vec();
    let mut spans = Vec::new();
    let mut deferred = loaded
        .config
        .input
        .command_aliases
        .values()
        .chain(loaded.dashboard.action.iter().map(|action| &action.command))
        .any(|id| plugin_owner_from_id(id).is_some_and(|owner| enabled.contains(owner)));
    let mut deferred_entries = Vec::new();
    for (offset, entry) in base.entries[base.bundled_count..].iter().enumerate() {
        let owner = plugin_owner_from_id(&entry.id)
            .or_else(|| entry.command.as_deref().and_then(plugin_owner_from_id))
            .or_else(|| entry.context.as_deref().and_then(plugin_owner_from_id));
        if owner.is_some_and(|owner| enabled.contains(owner)) {
            deferred = true;
            deferred_entries.push(entry);
        } else {
            entries.push(entry.clone());
            if let Some(span) = base.user_spans.get(offset) {
                spans.push(span.clone());
            }
        }
    }
    let policy = &loaded.config.input.keymaps;
    let leaders = literal_key_sequence("input.keymaps.leader", &policy.leader).and_then(|leader| {
        literal_key_sequence("input.keymaps.local_leader", &policy.local_leader)
            .map(|local_leader| (leader, local_leader))
    });
    let contexts = InputContexts::bundled();
    let mut groups = base.groups[..base.bundled_group_count].to_vec();
    let mut group_spans = Vec::new();
    for (offset, group) in base.groups[base.bundled_group_count..].iter().enumerate() {
        let owner = plugin_owner_from_id(&group.id)
            .or_else(|| group.context.as_deref().and_then(plugin_owner_from_id));
        let deferred_continuation = leaders.as_ref().is_ok_and(|(leader, local_leader)| {
            let Some(prefix) = group
                .prefix
                .as_deref()
                .and_then(|prefix| expanded_key_sequence(prefix, leader, local_leader).ok())
            else {
                return false;
            };
            let Some(context) = group
                .context
                .as_deref()
                .and_then(|context| configured_context(&contexts, context, &group.mode).ok())
            else {
                return false;
            };
            deferred_entries.iter().any(|entry| {
                entry.context.as_deref().is_some_and(|entry_context| {
                    configured_context(&contexts, entry_context, &entry.mode)
                        .is_ok_and(|entry_context| entry_context == context)
                }) && entry.keys.as_deref().is_some_and(|keys| {
                    expanded_key_sequence(keys, leader, local_leader)
                        .is_ok_and(|keys| keys.len() > prefix.len() && keys.starts_with(&prefix))
                })
            })
        });
        if owner.is_some_and(|owner| enabled.contains(owner)) || deferred_continuation {
            deferred = true;
        } else {
            groups.push(group.clone());
            if let Some(span) = base.user_group_spans.get(offset) {
                group_spans.push(span.clone());
            }
        }
    }
    base.entries = entries;
    base.groups = groups;
    base.user_spans = spans;
    base.user_group_spans = group_spans;
    (base, enabled, deferred)
}

pub(in crate::app) fn resolve_configuration(
    commands: &CommandRegistry<AppEffect>,
    mut loaded: LoadedConfig,
    strict: bool,
) -> Result<(ResolvedConfiguration, Option<String>), String> {
    if loaded.user_configuration {
        loaded.installed_plugins = crate::tools::installed_plugins(
            &crate::config::user_state_directory().join("packages"),
        );
    }
    let mut diagnostics = Vec::new();
    diagnostics.extend(loaded.diagnostic.iter().cloned());
    diagnostics.extend(loaded.keymaps.diagnostic.iter().cloned());
    if let Err(error) = validate_configured_commands(commands, &loaded) {
        diagnostics.push(error);
    }
    if strict && !diagnostics.is_empty() {
        return Err(diagnostics.join("; "));
    }
    let (base_keymaps, expected_plugin_indexes, mut deferred_plugin_keymaps) =
        base_safe_keymaps(&loaded);
    let (keymaps, keymap_diagnostic) = if strict {
        let (entries, bundled_count, groups, bundled_group_count) = keymap_entries(&base_keymaps);
        match compile_keymap_entries(
            commands,
            KeymapCompilation {
                entries,
                bundled_count,
                groups,
                bundled_group_count,
                user_path: &base_keymaps.path,
                user_spans: &base_keymaps.user_spans,
                user_group_spans: &base_keymaps.user_group_spans,
                policy: &loaded.config.input.keymaps,
            },
        ) {
            Ok(compiled) => (compiled, None),
            Err(errors) => return Err(errors.join("; ")),
        }
    } else {
        compile_keymaps(commands, &base_keymaps, &loaded.config.input.keymaps)
    };
    diagnostics.extend(keymap_diagnostic);
    let mut settings = loaded.config;
    let raw_windows = settings.windows.clone();
    if let Some(rule) = settings.windows.rules.iter().find(|rule| {
        plugin_owner_from_id(&rule.content_type)
            .is_some_and(|owner| !expected_plugin_indexes.contains(owner))
    }) {
        return Err(format!(
            "plugin window selector `{}` does not name an enabled plugin",
            rule.content_type
        ));
    }
    if settings.windows.rules.iter().any(|rule| {
        plugin_owner_from_id(&rule.content_type)
            .is_some_and(|owner| expected_plugin_indexes.contains(owner))
    }) {
        deferred_plugin_keymaps = true;
        settings
            .windows
            .rules
            .retain(|rule| !rule.content_type.starts_with("plugin."));
    }
    let keymap_source = if loaded.keymaps.bundled_count > 0
        && loaded.keymaps.entries.len() == loaded.keymaps.bundled_count
    {
        PathBuf::from("<bundled:keymaps>")
    } else {
        loaded.keymaps.path.clone()
    };
    let resolved = ResolvedConfiguration {
        settings,
        dashboard: loaded.dashboard,
        theme: loaded.theme,
        raw_keymaps: loaded.keymaps,
        base_keymaps,
        raw_windows,
        deferred_plugin_keymaps,
        expected_plugin_indexes,
        keymaps,
        sources: ConfigurationSources {
            settings: loaded.path,
            keymaps: keymap_source,
            dashboard: loaded.dashboard_path,
            theme: loaded.theme_path,
        },
        user_setting_keys: loaded.user_setting_keys,
        user_setting_spans: loaded.user_setting_spans,
        dashboard_spans: loaded.dashboard_spans,
        theme_spans: loaded.theme_spans,
    };
    Ok((
        resolved,
        (!diagnostics.is_empty()).then(|| diagnostics.join("; ")),
    ))
}

pub fn validate_loaded_configuration(loaded: LoadedConfig) -> Result<(), String> {
    let mut commands = CommandRegistry::default();
    register_builtin_commands(&mut commands)
        .map_err(|error| format!("built-in command registry failed: {error}"))?;
    register_app_commands(&mut commands)
        .map_err(|error| format!("application command registry failed: {error}"))?;
    resolve_configuration(&commands, loaded, true).map(|_| ())
}

pub(in crate::app) fn dashboard_actions(
    configuration: &ResolvedConfiguration,
) -> Vec<DashboardAction> {
    configuration
        .dashboard
        .action
        .iter()
        .filter(|action| action.visible)
        .map(|action| DashboardAction {
            command: action.command.clone(),
            label: action.label.clone(),
            icon: action.icon.clone(),
            key: action
                .show_binding
                .then(|| {
                    configuration
                        .keymaps
                        .binding_for_action("dashboard", &action.command, &action.arguments)
                        .map(str::to_owned)
                })
                .flatten(),
        })
        .collect()
}

pub(in crate::app) const fn configuration_reload_is_newer(current: u64, candidate: u64) -> bool {
    candidate > current
}

pub(in crate::app) fn configured_grapheme_width(
    grapheme: &str,
    ambiguous_width: crate::config::AmbiguousWidth,
) -> usize {
    let width = if ambiguous_width == crate::config::AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(grapheme)
    } else {
        UnicodeWidthStr::width(grapheme)
    };
    width.max(1)
}

pub(in crate::app) fn configured_text_width(
    text: &str,
    tab_width: usize,
    ambiguous_width: crate::config::AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            column += tab_width - (column % tab_width);
        } else {
            column += configured_grapheme_width(grapheme, ambiguous_width);
        }
    }
    column
}

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct ConfigurationSources {
    pub settings: PathBuf,
    pub keymaps: PathBuf,
    pub dashboard: PathBuf,
    pub theme: PathBuf,
}

#[derive(Clone)]
pub(crate) struct ResolvedConfiguration {
    pub settings: crate::config::Config,
    pub dashboard: crate::config::DashboardConfig,
    pub theme: crate::config::ThemeConfig,
    pub raw_keymaps: crate::config::LoadedKeymaps,
    pub base_keymaps: crate::config::LoadedKeymaps,
    pub raw_windows: crate::config::WindowsConfig,
    pub deferred_plugin_keymaps: bool,
    pub expected_plugin_indexes: BTreeSet<String>,
    pub keymaps: ResolvedKeymaps,
    pub(in crate::app) sources: ConfigurationSources,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub dashboard_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub theme_spans: BTreeMap<String, std::ops::Range<usize>>,
}

pub(in crate::app) fn resolved_toml_value(
    mut value: toml::Value,
    segments: &[String],
) -> Option<toml::Value> {
    for segment in segments {
        value = if let Some(table) = value.as_table() {
            table.get(segment)?.clone()
        } else {
            value
                .as_array()?
                .get(segment.parse::<usize>().ok()?)?
                .clone()
        };
    }
    Some(value)
}

impl ResolvedConfiguration {
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<crate::config::ConfigExplanation> {
        let (value, source, span, layered) = if let Some(key) = key.strip_prefix("theme.") {
            let segments = crate::config::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value = resolved_toml_value(toml::Value::try_from(&self.theme).ok()?, &segments)?;
            (
                value,
                &self.sources.theme,
                self.theme_spans.get(&canonical),
                false,
            )
        } else if let Some(key) = key.strip_prefix("dashboard.") {
            let segments = crate::config::config_key_segments(key)?;
            let canonical = segments.join(".");
            let value =
                resolved_toml_value(toml::Value::try_from(&self.dashboard).ok()?, &segments)?;
            (
                value,
                &self.sources.dashboard,
                self.dashboard_spans.get(&canonical),
                false,
            )
        } else {
            let segments = crate::config::config_key_segments(key)?;
            let value =
                resolved_toml_value(toml::Value::try_from(&self.settings).ok()?, &segments)?;
            let user_key = (1..=segments.len())
                .rev()
                .map(|length| segments[..length].join("."))
                .find(|ancestor| self.user_setting_keys.contains(ancestor));
            let source = if user_key.is_some() {
                &self.sources.settings
            } else {
                // Inherited values continue to identify the bundled declaration.
                static BUNDLED_SETTINGS: std::sync::LazyLock<PathBuf> =
                    std::sync::LazyLock::new(|| PathBuf::from("<bundled:config>"));
                &BUNDLED_SETTINGS
            };
            let span = user_key
                .as_ref()
                .and_then(|key| self.user_setting_spans.get(key));
            (value, source, span, true)
        };
        let mut final_source = source.display().to_string();
        if let Some(span) = span {
            final_source = format!("{final_source}:bytes {}..{}", span.start, span.end);
        }
        let bundled = final_source.starts_with("<bundled:");
        Some(crate::config::ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: if bundled {
                "bundled defaults".into()
            } else {
                final_source.clone()
            },
            contributors: if bundled {
                vec!["bundled defaults".into()]
            } else if layered {
                vec!["bundled defaults".into(), final_source]
            } else {
                vec![final_source]
            },
        })
    }
}

use crate::app::configuration::ResolvedKeymaps;
