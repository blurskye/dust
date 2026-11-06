//! Command references and typed arguments in configuration declarations.

use crate::app::commands::AppEffect;
use crate::config::LoadedConfig;
use crate::editor::CommandError;
use crate::editor::CommandInvocation;
use crate::editor::CommandRegistry;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use std::collections::BTreeSet;

pub(in crate::app) fn validate_configured_commands(
    commands: &CommandRegistry<AppEffect>,
    loaded: &LoadedConfig,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let command_ids = commands
        .specs()
        .map(|spec| spec.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    for spec in commands.specs() {
        match loaded.config.command_labels.get(spec.id.as_str()) {
            Some(label)
                if !label.is_empty() && !label.contains('\u{1b}') && label.len() <= 1_024 => {}
            Some(_) => errors.push(format!(
                "command label for `{}` is empty, unsafe, or oversized",
                spec.id
            )),
            None if spec.id.as_str().starts_with("plugin.") => {}
            None => errors.push(format!("missing command label for `{}`", spec.id)),
        }
    }
    let enabled_plugins = crate::app::configuration::resolve::enabled_plugin_owners(loaded);
    for extra in loaded.config.command_labels.keys().filter(|id| {
        !command_ids.contains(*id)
            && !crate::app::configuration::plugin_owner_from_id(id)
                .is_some_and(|owner| enabled_plugins.contains(owner))
    }) {
        errors.push(format!(
            "command label references unknown command `{extra}`"
        ));
    }
    for (kind, configured) in [
        ("text", &loaded.config.input.text_commands),
        ("paste", &loaded.config.input.paste_commands),
        (
            "character fallback",
            &loaded.config.input.character_fallback_commands,
        ),
    ] {
        for (context, command_id) in configured {
            let invocation =
                CommandInvocation::new(command_id, InvocationSource::Startup).map(|invocation| {
                    invocation.with_argument("text", CommandValue::String(String::new()))
                });
            if let Err(error) =
                invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
            {
                errors.push(format!("{kind} command for `{context}`: {error}"));
            }
        }
    }
    for (context, command_id) in &loaded.config.input.unmatched_commands {
        let invocation = CommandInvocation::new(command_id, InvocationSource::Startup);
        if let Err(error) =
            invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
        {
            errors.push(format!("unmatched command for `{context}`: {error}"));
        }
    }
    if let Err(error) = validate_command_references(
        commands,
        &loaded.config,
        &loaded.dashboard,
        &enabled_plugins,
    ) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(super) fn validate_command_references(
    commands: &CommandRegistry<AppEffect>,
    config: &crate::config::Config,
    dashboard: &crate::config::DashboardConfig,
    deferred_plugins: &BTreeSet<String>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let registered = commands
        .specs()
        .map(|spec| spec.id.as_str())
        .collect::<BTreeSet<_>>();
    let deferred = |id: &str| {
        !registered.contains(id)
            && crate::app::configuration::plugin_owner_from_id(id)
                .is_some_and(|owner| deferred_plugins.contains(owner))
    };
    for (alias, command_id) in &config.input.command_aliases {
        if alias.is_empty() || alias.chars().any(char::is_whitespace) || alias.contains('.') {
            errors.push(format!("invalid command alias `{alias}`"));
            continue;
        }
        if !config.command_labels.get(command_id).is_some_and(|label| {
            !label.is_empty() && !label.contains('\u{1b}') && label.len() <= 1_024
        }) {
            errors.push(format!(
                "command alias `{alias}` requires a valid command label for `{command_id}`"
            ));
        }
        if let Err(error) = CommandInvocation::new(command_id, InvocationSource::Startup) {
            errors.push(format!("command alias `{alias}`: {error}"));
        } else if !registered.contains(command_id.as_str()) && !deferred(command_id) {
            errors.push(format!(
                "command alias `{alias}` references unknown command `{command_id}`"
            ));
        }
    }
    for action in &dashboard.action {
        let invocation = CommandInvocation::new(&action.command, InvocationSource::Startup)
            .and_then(|mut invocation| {
                for (name, value) in &action.arguments {
                    let value = configuration_value(value).map_err(|()| {
                        CommandError::InvalidArguments(format!(
                            "dashboard argument `{name}` has unsupported type"
                        ))
                    })?;
                    invocation.arguments.insert(name.clone(), value);
                }
                Ok(invocation)
            });
        if let Err(error) = invocation.and_then(|invocation| {
            if deferred(invocation.id.as_str()) {
                Ok(())
            } else {
                commands.validate_invocation_shape(&invocation)
            }
        }) {
            errors.push(format!("dashboard action `{}`: {error}", action.label));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(in crate::app) fn configuration_value(value: &toml::Value) -> Result<CommandValue, ()> {
    match value {
        toml::Value::String(value) => Ok(CommandValue::String(value.clone())),
        toml::Value::Boolean(value) => Ok(CommandValue::Bool(*value)),
        toml::Value::Integer(value) => Ok(CommandValue::Integer(*value)),
        _ => Err(()),
    }
}

pub(in crate::app) fn invocation_fingerprint(invocation: &CommandInvocation) -> String {
    let mut fingerprint = invocation.id.as_str().to_owned();
    for (name, value) in &invocation.arguments {
        fingerprint.push('|');
        fingerprint.push_str(&name.len().to_string());
        fingerprint.push(':');
        fingerprint.push_str(name);
        fingerprint.push('=');
        match value {
            CommandValue::String(value) => {
                fingerprint.push_str("string:");
                fingerprint.push_str(&value.len().to_string());
                fingerprint.push(':');
                fingerprint.push_str(value);
            }
            CommandValue::Bool(value) => {
                fingerprint.push_str(if *value { "bool:true" } else { "bool:false" })
            }
            CommandValue::Integer(value) => {
                fingerprint.push_str("integer:");
                fingerprint.push_str(&value.to_string());
            }
            CommandValue::Number(_) | CommandValue::List(_) | CommandValue::Record(_) => {
                fingerprint.push_str(&format!("{value:?}"));
            }
        }
    }
    fingerprint
}

pub(in crate::app) fn keymap_entries(
    loaded: &crate::config::LoadedKeymaps,
) -> (
    &[crate::config::KeymapEntry],
    usize,
    &[crate::config::KeyGroupEntry],
    usize,
) {
    (
        &loaded.entries,
        loaded.bundled_count,
        &loaded.groups,
        loaded.bundled_group_count,
    )
}
