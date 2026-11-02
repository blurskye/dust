use super::{
    AppEffect, BTreeSet, CommandRegistry, ConfigurationSources, DashboardAction, KeymapCompilation,
    LoadedConfig, PathBuf, ResolvedConfiguration, UnicodeSegmentation, UnicodeWidthStr,
    compile_keymap_entries, compile_keymaps, keymap_entries, plugin_owner_from_id,
    register_app_commands, register_builtin_commands, validate_configured_commands,
};

pub(in crate::editor::session) fn base_safe_keymaps(
    loaded: &LoadedConfig,
) -> (crate::config::LoadedKeymaps, BTreeSet<String>, bool) {
    let enabled = loaded
        .config
        .plugins
        .entries
        .iter()
        .filter(|(_, entry)| entry.enabled)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut base = loaded.keymaps.clone();
    let mut entries = base.entries[..base.bundled_count].to_vec();
    let mut spans = Vec::new();
    let mut deferred = false;
    for (offset, entry) in base.entries[base.bundled_count..].iter().enumerate() {
        let owner = plugin_owner_from_id(&entry.id)
            .or_else(|| entry.command.as_deref().and_then(plugin_owner_from_id))
            .or_else(|| entry.context.as_deref().and_then(plugin_owner_from_id));
        if owner.is_some_and(|owner| enabled.contains(owner)) {
            deferred = true;
        } else {
            entries.push(entry.clone());
            if let Some(span) = base.user_spans.get(offset) {
                spans.push(span.clone());
            }
        }
    }
    let mut groups = base.groups[..base.bundled_group_count].to_vec();
    let mut group_spans = Vec::new();
    for (offset, group) in base.groups[base.bundled_group_count..].iter().enumerate() {
        let owner = plugin_owner_from_id(&group.id)
            .or_else(|| group.context.as_deref().and_then(plugin_owner_from_id));
        if owner.is_some_and(|owner| enabled.contains(owner)) {
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

pub(in crate::editor::session) fn resolve_configuration(
    commands: &CommandRegistry<AppEffect>,
    loaded: LoadedConfig,
    strict: bool,
) -> Result<(ResolvedConfiguration, Option<String>), String> {
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
        let (entries, bundled_count, groups, bundled_group_count) =
            keymap_entries(&base_keymaps);
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

pub(in crate::editor::session) fn dashboard_actions(
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

pub(in crate::editor::session) const fn configuration_generation_is_newer(
    current: u64,
    candidate: u64,
) -> bool {
    candidate > current
}

pub(in crate::editor::session) fn configured_grapheme_width(
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

pub(in crate::editor::session) fn configured_byte_offset_at_display_column(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: crate::config::AmbiguousWidth,
) -> usize {
    let mut column = 0usize;
    let mut byte = 0usize;
    for (relative, grapheme) in text.grapheme_indices(true) {
        let width = if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            configured_grapheme_width(grapheme, ambiguous_width)
        };
        if column.saturating_add(width) > target {
            return relative;
        }
        column = column.saturating_add(width);
        byte = relative.saturating_add(grapheme.len());
    }
    byte
}

pub(in crate::editor::session) fn configured_text_width(
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
