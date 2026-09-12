use super::{
    AppEffect, BTreeSet, CommandError, CommandInvocation, CommandRegistry, CommandValue,
    InvocationSource, LoadedConfig,
};

pub(in crate::editor::session) fn validate_configured_commands(
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
            None => errors.push(format!("missing command label for `{}`", spec.id)),
        }
    }
    for extra in loaded
        .config
        .command_labels
        .keys()
        .filter(|id| !command_ids.contains(*id))
    {
        errors.push(format!(
            "command label references unknown command `{extra}`"
        ));
    }
    let source = include_str!("mod.rs");
    let marker = concat!(".", "message(");
    let mut remainder = source;
    while let Some(index) = remainder.find(marker) {
        remainder = &remainder[index + marker.len()..];
        let trimmed = remainder.trim_start();
        if let Some(trimmed) = trimmed.strip_prefix('"') {
            if let Some(end) = trimmed.find('"') {
                let key = &trimmed[..end];
                if !loaded.config.messages.contains_key(key) {
                    errors.push(format!(
                        "missing message `{key}` used by application surface"
                    ));
                }
            }
        }
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
    for (alias, command_id) in &loaded.config.input.command_aliases {
        if loaded
            .config
            .input
            .disabled_command_aliases
            .iter()
            .any(|disabled| disabled == alias)
        {
            continue;
        }
        if alias.is_empty() || alias.chars().any(char::is_whitespace) || alias.contains('.') {
            errors.push(format!("invalid command alias `{alias}`"));
            continue;
        }
        let invocation = CommandInvocation::new(command_id, InvocationSource::Startup);
        if let Err(error) = invocation {
            errors.push(format!("command alias `{alias}`: {error}"));
        } else if !commands.specs().any(|spec| spec.id.as_str() == command_id) {
            errors.push(format!(
                "command alias `{alias}` references unknown command `{command_id}`"
            ));
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
    for action in &loaded.dashboard.action {
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
        if let Err(error) =
            invocation.and_then(|invocation| commands.validate_invocation_shape(&invocation))
        {
            errors.push(format!("dashboard action `{}`: {error}", action.label));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(in crate::editor::session) fn configuration_value(
    value: &toml::Value,
) -> Result<CommandValue, ()> {
    match value {
        toml::Value::String(value) => Ok(CommandValue::String(value.clone())),
        toml::Value::Boolean(value) => Ok(CommandValue::Bool(*value)),
        toml::Value::Integer(value) => Ok(CommandValue::Integer(*value)),
        _ => Err(()),
    }
}

pub(in crate::editor::session) fn invocation_fingerprint(invocation: &CommandInvocation) -> String {
    let mut fingerprint = invocation.id.as_str().to_owned();
    for (name, value) in &invocation.arguments {
        fingerprint.push('|');
        fingerprint.push_str(name);
        fingerprint.push('=');
        match value {
            CommandValue::String(value) => {
                fingerprint.push_str("string:");
                fingerprint.push_str(value);
            }
            CommandValue::Bool(value) => {
                fingerprint.push_str(if *value { "bool:true" } else { "bool:false" })
            }
            CommandValue::Integer(value) => {
                fingerprint.push_str("integer:");
                fingerprint.push_str(&value.to_string());
            }
        }
    }
    fingerprint
}

pub(in crate::editor::session) fn keymap_entries_for_policy<'a>(
    loaded: &'a crate::config::LoadedKeymaps,
    policy: &crate::config::KeymapPolicyConfig,
) -> (
    &'a [crate::config::KeymapEntry],
    usize,
    &'a [crate::config::KeyGroupEntry],
    usize,
) {
    if policy.load_bundled_defaults {
        (
            &loaded.entries,
            loaded.bundled_count,
            &loaded.groups,
            loaded.bundled_group_count,
        )
    } else {
        (
            &loaded.entries[loaded.bundled_count..],
            0,
            &loaded.groups[loaded.bundled_group_count..],
            0,
        )
    }
}
