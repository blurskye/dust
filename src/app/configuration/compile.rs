//! Keymap compilation with bundled, plugin and user precedence.

use crate::app::commands::AppEffect;
use crate::app::configuration::KeymapInspection;
use crate::app::configuration::KeymapOperation;
use crate::app::configuration::ResolvedKeyGroup;
use crate::app::configuration::ResolvedKeymaps;
use crate::app::configuration::ReverseBinding;
use crate::app::configuration::configured_context;
use crate::app::configuration::invocation_fingerprint;
use crate::app::configuration::keymap_entries;
use crate::editor::CommandInvocation;
use crate::editor::CommandRegistry;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::InputContextLayer;
use crate::editor::input::InputContexts;
use crate::editor::input::KeyEvent;
use crate::editor::input::KeymapLayer;
use crate::editor::input::KeymapSequence;
use crate::editor::input::MappingResolution;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::str::FromStr;

pub(crate) fn compile_keymaps(
    commands: &CommandRegistry<AppEffect>,
    loaded: &crate::config::LoadedKeymaps,
    policy: &crate::config::KeymapPolicyConfig,
) -> (ResolvedKeymaps, Option<String>) {
    let (entries, bundled_count, groups, bundled_group_count) = keymap_entries(loaded);
    match compile_keymap_entries(
        commands,
        KeymapCompilation {
            entries,
            bundled_count,
            groups,
            bundled_group_count,
            user_path: &loaded.path,
            user_spans: &loaded.user_spans,
            user_group_spans: &loaded.user_group_spans,
            policy,
        },
    ) {
        Ok(keymaps) => (keymaps, None),
        Err(diagnostics) if bundled_count > 0 || bundled_group_count > 0 => {
            match compile_keymap_entries(
                commands,
                KeymapCompilation {
                    entries: &entries[..bundled_count],
                    bundled_count,
                    groups: &groups[..bundled_group_count],
                    bundled_group_count,
                    user_path: &loaded.path,
                    user_spans: &[],
                    user_group_spans: &[],
                    policy,
                },
            ) {
                Ok(mut bundled) => {
                    let message = format!(
                        "invalid {}; keeping bundled keymaps: {}",
                        loaded.path.display(),
                        diagnostics.join("; ")
                    );
                    // `:keymap.conflicts` answers for what the user wrote, not what replaced it.
                    bundled.conflicts.extend(diagnostics);
                    (bundled, Some(message))
                }
                Err(_) => (
                    ResolvedKeymaps::default(),
                    Some(format!(
                        "invalid keymap policy for {}: {}",
                        loaded.path.display(),
                        diagnostics.join("; ")
                    )),
                ),
            }
        }
        Err(diagnostics) => (
            ResolvedKeymaps::default(),
            Some(format!(
                "invalid {}: {}",
                loaded.path.display(),
                diagnostics.join("; ")
            )),
        ),
    }
}

pub(in crate::app) fn literal_key_sequence(
    name: &str,
    source: &str,
) -> Result<Vec<KeyEvent>, String> {
    let tokens = source.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if source.len() > 64 || tokens.len() > 4 {
        return Err(format!("{name} must contain at most 4 keys and 64 bytes"));
    }
    if source.chars().any(char::is_control) {
        return Err(format!("{name} must not contain control characters"));
    }
    if tokens
        .iter()
        .any(|token| matches!(*token, "<leader>" | "<localleader>"))
    {
        return Err(format!(
            "{name} must be a literal sequence without leader tokens"
        ));
    }
    tokens
        .into_iter()
        .map(|token| KeyEvent::from_str(token).map_err(|error| format!("{name}: {error}")))
        .collect()
}

pub(in crate::app) fn expanded_key_sequence(
    source: &str,
    leader: &[KeyEvent],
    local_leader: &[KeyEvent],
) -> Result<Vec<KeyEvent>, String> {
    let mut expanded = Vec::new();
    for token in source.split_whitespace() {
        match token {
            "<leader>" => expanded.extend_from_slice(leader),
            "<localleader>" => expanded.extend_from_slice(local_leader),
            _ => expanded.push(KeyEvent::from_str(token).map_err(|error| error.to_string())?),
        }
        if expanded.len() > 64 {
            return Err("expanded key sequence exceeds 64 keys".into());
        }
    }
    if expanded.is_empty() {
        Err("key sequence is empty".into())
    } else {
        Ok(expanded)
    }
}

pub(in crate::app) fn display_key_sequence(sequence: &[KeyEvent]) -> String {
    sequence
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(in crate::app) fn display_key_declaration(source: &str, effective: &str) -> String {
    if source == effective {
        format!("`{source}`")
    } else {
        format!("`{source}` (expanded to `{effective}`)")
    }
}

pub(in crate::app) fn validate_keymap_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 {
        return Err("id must contain between 1 and 128 bytes".into());
    }
    let segments = id.split('.').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            let mut characters = segment.chars();
            !characters
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
                || characters.any(|character| {
                    !character.is_ascii_lowercase()
                        && !character.is_ascii_digit()
                        && character != '-'
                })
        })
    {
        return Err(
            "id must have at least two dot-separated lowercase ASCII segments; each segment starts with a letter and then uses letters, digits, or `-`"
                .into(),
        );
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct KeymapContributionOrigin {
    layer: InputContextLayer,
    owner: Option<String>,
    plugin_load: u64,
    source: String,
    declaration: usize,
}

#[derive(Clone)]
pub(in crate::app) struct EffectiveKeymapDeclaration {
    id: String,
    context: String,
    mode: String,
    keys: String,
    command: Option<String>,
    arguments: BTreeMap<String, toml::Value>,
    operation: KeymapOperation,
    desc: Option<String>,
    origin: KeymapContributionOrigin,
}

pub(in crate::app) fn regular_keymap_declaration(
    entry: &crate::config::KeymapEntry,
    origin: KeymapContributionOrigin,
) -> Result<EffectiveKeymapDeclaration, String> {
    let operations = usize::from(entry.command.is_some())
        + usize::from(entry.unmap)
        + usize::from(entry.pass_through);
    if entry.disable || operations != 1 {
        return Err(
            "must choose one command, exact-sequence unmap, exact-sequence pass_through, or identity-only disable"
                .into(),
        );
    }
    if !entry.arguments.is_empty() && entry.command.is_none() {
        return Err("arguments require a command declaration".into());
    }
    let context = entry
        .context
        .as_ref()
        .filter(|context| !context.is_empty())
        .ok_or_else(|| "command/unmap/pass_through declaration requires `context`".to_owned())?;
    let keys = entry
        .keys
        .as_ref()
        .filter(|keys| !keys.is_empty())
        .ok_or_else(|| "command/unmap/pass_through declaration requires `keys`".to_owned())?;
    if entry.desc.as_ref().is_some_and(|desc| {
        desc.is_empty() || desc.chars().count() > 64 || desc.chars().any(char::is_control)
    }) {
        return Err("desc must contain 1..=64 non-control characters".into());
    }
    Ok(EffectiveKeymapDeclaration {
        id: entry.id.clone(),
        context: context.clone(),
        mode: entry.mode.clone(),
        keys: keys.clone(),
        command: entry.command.clone(),
        arguments: entry.arguments.clone(),
        operation: if entry.command.is_some() {
            KeymapOperation::Command
        } else if entry.unmap {
            KeymapOperation::Unmap
        } else {
            KeymapOperation::PassThrough
        },
        desc: entry.desc.clone(),
        origin,
    })
}

pub(in crate::app) fn identity_disable_is_well_formed(entry: &crate::config::KeymapEntry) -> bool {
    entry.disable
        && entry.command.is_none()
        && !entry.unmap
        && !entry.pass_through
        && entry.context.is_none()
        && entry.mode.is_empty()
        && entry.keys.is_none()
        && entry.arguments.is_empty()
        && entry.desc.is_none()
}

pub(in crate::app) fn resolve_keymap_identities(
    entries: &[crate::config::KeymapEntry],
    origins: &[KeymapContributionOrigin],
) -> Result<Vec<EffectiveKeymapDeclaration>, Vec<String>> {
    let mut diagnostics = Vec::new();
    let mut slots = Vec::<EffectiveKeymapDeclaration>::new();
    let mut identities = HashMap::<String, usize>::new();
    for (source_index, entry) in entries.iter().enumerate() {
        let origin = origins[source_index].clone();
        if let Err(error) = validate_keymap_id(&entry.id) {
            diagnostics.push(format!(
                "keymap {} has invalid id `{}`: {error}",
                source_index + 1,
                entry.id
            ));
            continue;
        }
        if let Some(slot) = identities.get(&entry.id).copied() {
            let previous = &slots[slot];
            if previous.origin.layer == origin.layer {
                let detail = if previous.origin.owner != origin.owner {
                    "across plugin owners"
                } else {
                    "in the same contribution layer"
                };
                diagnostics.push(format!(
                    "keymap {} duplicates id `{}` from keymap {} {detail}",
                    source_index + 1,
                    entry.id,
                    previous.origin.declaration
                ));
                continue;
            }
            if previous.origin.layer > origin.layer {
                diagnostics.push(format!(
                    "keymap {} id `{}` has lower precedence than its existing contribution",
                    source_index + 1,
                    entry.id
                ));
                continue;
            }
            if identity_disable_is_well_formed(entry) {
                slots[slot] = EffectiveKeymapDeclaration {
                    id: entry.id.clone(),
                    context: previous.context.clone(),
                    mode: previous.mode.clone(),
                    keys: previous.keys.clone(),
                    command: None,
                    arguments: BTreeMap::new(),
                    operation: KeymapOperation::Disable,
                    desc: None,
                    origin,
                };
                continue;
            }
            if entry.disable {
                diagnostics.push(format!(
                    "keymap {} identity disable for id `{}` must contain only `id` and `disable = true`",
                    source_index + 1,
                    entry.id
                ));
                continue;
            }
            match regular_keymap_declaration(entry, origin) {
                Ok(replacement) => slots[slot] = replacement,
                Err(error) => diagnostics.push(format!(
                    "keymap {} replacement for id `{}` {error}",
                    source_index + 1,
                    entry.id
                )),
            }
            continue;
        }
        if entry.disable {
            let detail = if identity_disable_is_well_formed(entry) {
                "targets no lower contribution"
            } else {
                "must contain only `id` and `disable = true`"
            };
            diagnostics.push(format!(
                "keymap {} identity disable for id `{}` {detail}",
                source_index + 1,
                entry.id
            ));
            continue;
        }
        match regular_keymap_declaration(entry, origin) {
            Ok(declaration) => {
                identities.insert(entry.id.clone(), slots.len());
                slots.push(declaration);
            }
            Err(error) => diagnostics.push(format!(
                "keymap {} id `{}` {error}",
                source_index + 1,
                entry.id
            )),
        }
    }
    if diagnostics.is_empty() {
        slots.sort_by_key(|entry| entry.origin.layer);
        Ok(slots)
    } else {
        Err(diagnostics)
    }
}

#[derive(Clone)]
pub(in crate::app) struct EffectiveKeyGroupDeclaration {
    id: String,
    context: String,
    mode: String,
    prefix: String,
    label: String,
    icon: Option<String>,
    order: i32,
    origin: KeymapContributionOrigin,
}

pub(in crate::app) fn concrete_key_group(
    entry: &crate::config::KeyGroupEntry,
    origin: KeymapContributionOrigin,
) -> Result<EffectiveKeyGroupDeclaration, String> {
    if entry.disable {
        return Err("identity-only disable must contain only `id` and `disable = true`".into());
    }
    let context = entry
        .context
        .as_ref()
        .filter(|context| !context.is_empty())
        .ok_or_else(|| "requires `context`".to_owned())?;
    let prefix = entry
        .prefix
        .as_ref()
        .filter(|prefix| !prefix.is_empty())
        .ok_or_else(|| "requires `prefix`".to_owned())?;
    let label = entry
        .label
        .as_ref()
        .filter(|label| !label.is_empty())
        .ok_or_else(|| "requires `label`".to_owned())?;
    if prefix.len() > 1_024 {
        return Err("prefix exceeds 1024 bytes".into());
    }
    if label.len() > 256
        || label.chars().count() > 128
        || unicode_width::UnicodeWidthStr::width(label.as_str()) > 128
        || label.chars().any(char::is_control)
    {
        return Err(
            "label must contain 1..=128 non-control characters and at most 256 bytes".into(),
        );
    }
    if entry.icon.as_ref().is_some_and(|icon| {
        icon.is_empty()
            || icon.len() > 32
            || icon.chars().count() > 8
            || !(1..=8).contains(&unicode_width::UnicodeWidthStr::width(icon.as_str()))
            || icon.chars().any(char::is_control)
    }) {
        return Err("icon must contain 1..=8 non-control characters and at most 32 bytes".into());
    }
    if !(-100_000..=100_000).contains(&entry.order) {
        return Err("order is outside -100000..=100000".into());
    }
    Ok(EffectiveKeyGroupDeclaration {
        id: entry.id.clone(),
        context: context.clone(),
        mode: entry.mode.clone(),
        prefix: prefix.clone(),
        label: label.clone(),
        icon: entry.icon.clone(),
        order: entry.order,
        origin,
    })
}

pub(in crate::app) fn key_group_disable_is_well_formed(
    entry: &crate::config::KeyGroupEntry,
) -> bool {
    entry.disable
        && entry.context.is_none()
        && entry.mode.is_empty()
        && entry.prefix.is_none()
        && entry.label.is_none()
        && entry.icon.is_none()
        && entry.order == 0
}

pub(in crate::app) fn resolve_key_group_identities(
    entries: &[crate::config::KeyGroupEntry],
    origins: &[KeymapContributionOrigin],
) -> Result<Vec<EffectiveKeyGroupDeclaration>, Vec<String>> {
    let mut diagnostics = Vec::new();
    let mut slots = Vec::<Option<EffectiveKeyGroupDeclaration>>::new();
    let mut identities = HashMap::<String, usize>::new();
    let mut identity_origins = HashMap::<String, KeymapContributionOrigin>::new();
    for (source_index, entry) in entries.iter().enumerate() {
        let origin = origins[source_index].clone();
        if let Err(error) = validate_keymap_id(&entry.id) {
            diagnostics.push(format!(
                "key group {} has invalid id `{}`: {error}",
                source_index + 1,
                entry.id
            ));
            continue;
        }
        if let Some(slot) = identities.get(&entry.id).copied() {
            let previous = &identity_origins[&entry.id];
            if previous.layer == origin.layer {
                diagnostics.push(format!(
                    "key group {} duplicates id `{}` in the same contribution layer",
                    source_index + 1,
                    entry.id
                ));
                continue;
            }
            if previous.layer > origin.layer {
                diagnostics.push(format!(
                    "key group {} id `{}` has lower precedence than its existing contribution",
                    source_index + 1,
                    entry.id
                ));
                continue;
            }
            if entry.disable {
                if key_group_disable_is_well_formed(entry) {
                    slots[slot] = None;
                    identity_origins.insert(entry.id.clone(), origin);
                } else {
                    diagnostics.push(format!(
                        "key group {} identity disable for id `{}` must contain only `id` and `disable = true`",
                        source_index + 1,
                        entry.id
                    ));
                }
                continue;
            }
            match concrete_key_group(entry, origin.clone()) {
                Ok(replacement) => {
                    slots[slot] = Some(replacement);
                    identity_origins.insert(entry.id.clone(), origin);
                }
                Err(error) => diagnostics.push(format!(
                    "key group {} replacement for id `{}` {error}",
                    source_index + 1,
                    entry.id
                )),
            }
            continue;
        }
        if entry.disable {
            let detail = if key_group_disable_is_well_formed(entry) {
                "targets no lower contribution"
            } else {
                "must contain only `id` and `disable = true`"
            };
            diagnostics.push(format!(
                "key group {} identity disable for id `{}` {detail}",
                source_index + 1,
                entry.id
            ));
            continue;
        }
        match concrete_key_group(entry, origin.clone()) {
            Ok(group) => {
                identities.insert(entry.id.clone(), slots.len());
                identity_origins.insert(entry.id.clone(), origin);
                slots.push(Some(group));
            }
            Err(error) => diagnostics.push(format!(
                "key group {} id `{}` {error}",
                source_index + 1,
                entry.id
            )),
        }
    }
    if diagnostics.is_empty() {
        Ok(slots.into_iter().flatten().collect())
    } else {
        Err(diagnostics)
    }
}

pub(in crate::app) fn standard_contribution_origins(
    count: usize,
    bundled_count: usize,
    user_path: &std::path::Path,
    user_spans: &[std::ops::Range<usize>],
    kind: &str,
) -> Vec<KeymapContributionOrigin> {
    (0..count)
        .map(|index| {
            if index < bundled_count {
                KeymapContributionOrigin {
                    layer: InputContextLayer::Bundled,
                    owner: None,
                    plugin_load: 0,
                    source: format!("<bundled:keymaps>:{kind} {}", index + 1),
                    declaration: index + 1,
                }
            } else {
                let declaration = index + 1 - bundled_count;
                let source = user_spans.get(declaration - 1).map_or_else(
                    || format!("{}:{kind} {declaration}", user_path.display()),
                    |span| {
                        format!(
                            "{}:bytes {}..{} ({kind} {declaration})",
                            user_path.display(),
                            span.start,
                            span.end
                        )
                    },
                );
                KeymapContributionOrigin {
                    layer: InputContextLayer::User,
                    owner: None,
                    plugin_load: 0,
                    source,
                    declaration,
                }
            }
        })
        .collect()
}

pub(in crate::app) struct KeymapCompilation<'a> {
    pub(crate) entries: &'a [crate::config::KeymapEntry],
    pub(crate) bundled_count: usize,
    pub(crate) groups: &'a [crate::config::KeyGroupEntry],
    pub(crate) bundled_group_count: usize,
    pub(crate) user_path: &'a std::path::Path,
    pub(crate) user_spans: &'a [std::ops::Range<usize>],
    pub(crate) user_group_spans: &'a [std::ops::Range<usize>],
    pub(crate) policy: &'a crate::config::KeymapPolicyConfig,
}

pub(in crate::app) fn compile_keymap_entries(
    commands: &CommandRegistry<AppEffect>,
    source: KeymapCompilation<'_>,
) -> Result<ResolvedKeymaps, Vec<String>> {
    let entry_origins = standard_contribution_origins(
        source.entries.len(),
        source.bundled_count,
        source.user_path,
        source.user_spans,
        "declaration",
    );
    let group_origins = standard_contribution_origins(
        source.groups.len(),
        source.bundled_group_count,
        source.user_path,
        source.user_group_spans,
        "key-group",
    );
    compile_typed_keymap_entries(
        commands,
        source.entries,
        &entry_origins,
        source.groups,
        &group_origins,
        &InputContexts::bundled(),
        source.policy,
    )
}

pub(crate) fn compile_keymaps_with_plugin_catalog(
    commands: &CommandRegistry<AppEffect>,
    loaded: &crate::config::LoadedKeymaps,
    catalog: &crate::plugins::PluginContributionCatalog,
    policy: &crate::config::KeymapPolicyConfig,
) -> Result<ResolvedKeymaps, Vec<String>> {
    let contexts = catalog
        .context_snapshot()
        .map_err(|error| vec![error.to_string()])?;
    let bundled_count = loaded.bundled_count;
    let bundled_group_count = loaded.bundled_group_count;
    let mut entries = loaded.entries[..loaded.bundled_count].to_vec();
    let mut entry_origins = standard_contribution_origins(
        bundled_count,
        bundled_count,
        &loaded.path,
        &[],
        "declaration",
    );
    for index in catalog.ordered() {
        for (offset, entry) in index.keymaps.iter().enumerate() {
            entries.push(crate::config::KeymapEntry {
                id: entry.id.clone(),
                context: Some(entry.context.clone()),
                mode: entry.mode.clone(),
                keys: Some(entry.keys.clone()),
                command: Some(entry.command.clone()),
                arguments: entry.arguments.clone(),
                unmap: false,
                disable: false,
                pass_through: false,
                desc: None,
            });
            entry_origins.push(KeymapContributionOrigin {
                layer: InputContextLayer::Plugin,
                owner: Some(index.owner.clone()),
                plugin_load: index.plugin_load,
                source: format!("{}:plugin-keymap {}", index.source, offset + 1),
                declaration: offset + 1,
            });
        }
    }
    entries.extend_from_slice(&loaded.entries[loaded.bundled_count..]);
    entry_origins.extend(standard_contribution_origins(
        loaded.entries.len() - loaded.bundled_count,
        0,
        &loaded.path,
        &loaded.user_spans,
        "declaration",
    ));
    let mut groups = loaded.groups[..loaded.bundled_group_count].to_vec();
    let mut group_origins = standard_contribution_origins(
        bundled_group_count,
        bundled_group_count,
        &loaded.path,
        &[],
        "key-group",
    );
    for index in catalog.ordered() {
        for (offset, group) in index.key_groups.iter().enumerate() {
            groups.push(crate::config::KeyGroupEntry {
                id: group.id.clone(),
                context: Some(group.context.clone()),
                mode: group.mode.clone(),
                prefix: Some(group.prefix.clone()),
                label: Some(group.label.clone()),
                icon: group.icon.clone(),
                order: group.order,
                disable: false,
            });
            group_origins.push(KeymapContributionOrigin {
                layer: InputContextLayer::Plugin,
                owner: Some(index.owner.clone()),
                plugin_load: index.plugin_load,
                source: format!("{}:plugin-key-group {}", index.source, offset + 1),
                declaration: offset + 1,
            });
        }
    }
    groups.extend_from_slice(&loaded.groups[loaded.bundled_group_count..]);
    group_origins.extend(standard_contribution_origins(
        loaded.groups.len() - loaded.bundled_group_count,
        0,
        &loaded.path,
        &loaded.user_group_spans,
        "key-group",
    ));
    compile_typed_keymap_entries(
        commands,
        &entries,
        &entry_origins,
        &groups,
        &group_origins,
        &contexts,
        policy,
    )
}

pub(in crate::app) fn compile_typed_keymap_entries(
    commands: &CommandRegistry<AppEffect>,
    entries: &[crate::config::KeymapEntry],
    entry_origins: &[KeymapContributionOrigin],
    groups: &[crate::config::KeyGroupEntry],
    group_origins: &[KeymapContributionOrigin],
    contexts: &InputContexts,
    policy: &crate::config::KeymapPolicyConfig,
) -> Result<ResolvedKeymaps, Vec<String>> {
    let mut result = ResolvedKeymaps::default();
    let mut diagnostics = Vec::new();
    // A key declared twice meets again in every context it expands to; say so once.
    let mut key_conflicts: Vec<(String, Vec<String>)> = Vec::new();
    if entries.len() != entry_origins.len() || groups.len() != group_origins.len() {
        return Err(vec!["keymap contribution provenance is incomplete".into()]);
    }
    let leader = match literal_key_sequence("input.keymaps.leader", &policy.leader) {
        Ok(sequence) => sequence,
        Err(error) => {
            diagnostics.push(error);
            return Err(diagnostics);
        }
    };
    let local_leader =
        match literal_key_sequence("input.keymaps.local_leader", &policy.local_leader) {
            Ok(sequence) => sequence,
            Err(error) => {
                diagnostics.push(error);
                return Err(diagnostics);
            }
        };
    let effective_entries = resolve_keymap_identities(entries, entry_origins)?;
    let mut sequence_declarations =
        HashMap::<(String, Vec<KeyEvent>, InputContextLayer), usize>::new();
    let mut effective_declarations = HashMap::<(String, Vec<KeyEvent>), usize>::new();
    for (index, entry) in effective_entries.iter().enumerate() {
        let context = match configured_context(contexts, &entry.context, &entry.mode) {
            Ok(context) => context,
            Err(error) => {
                diagnostics.push(format!(
                    "keymap id `{}` from declaration {}: {error}",
                    entry.id, entry.origin.declaration
                ));
                continue;
            }
        };
        let sequence = match expanded_key_sequence(&entry.keys, &leader, &local_leader) {
            Ok(sequence) => sequence,
            Err(error) => {
                diagnostics.push(format!(
                    "keymap id `{}` from declaration {}: {error}",
                    entry.id, entry.origin.declaration
                ));
                continue;
            }
        };
        let effective_keys = display_key_sequence(&sequence);
        let contribution_layer = entry.origin.layer;
        if let Some(previous) =
            effective_declarations.insert((context.clone(), sequence.clone()), index)
            && effective_entries[previous].origin.layer != contribution_layer
        {
            let declared = display_key_declaration(&entry.keys, &effective_keys);
            let previous_entry = &effective_entries[previous];
            let previous_declared = display_key_declaration(&previous_entry.keys, &effective_keys);
            result.conflicts.push(format!(
                "keymap id `{}` {declared} shadows id `{}` {previous_declared} in `{context}`",
                entry.id, previous_entry.id
            ));
        }
        if let Some(previous) = sequence_declarations.insert(
            (context.clone(), sequence.clone(), contribution_layer),
            index,
        ) {
            let declared = display_key_declaration(&entry.keys, &effective_keys);
            let previous_entry = &effective_entries[previous];
            let previous_declared = display_key_declaration(&previous_entry.keys, &effective_keys);
            // A `global` entry is one entry per context, and each of those ids ends in its context.
            let suffix = format!(".{}", context.replace(['.', '_'], "-"));
            let base = |id: &'_ str| id.strip_suffix(&suffix).unwrap_or(id).to_owned();
            let conflict = format!(
                "keymap id `{}` {declared} conflicts with id `{}` {previous_declared}",
                base(&entry.id),
                base(&previous_entry.id)
            );
            match key_conflicts.iter_mut().find(|(text, _)| *text == conflict) {
                Some((_, contexts)) => contexts.push(context.clone()),
                None => key_conflicts.push((conflict, vec![context.clone()])),
            }
            continue;
        }
        if contribution_layer != InputContextLayer::Bundled
            && entry.operation != KeymapOperation::PassThrough
        {
            result.inspections.retain(|inspection| {
                inspection.context != context || inspection.keys != effective_keys
            });
        }
        let source = entry.origin.source.clone();
        let layer = match contribution_layer {
            InputContextLayer::Bundled => KeymapLayer::Global,
            InputContextLayer::Plugin => KeymapLayer::PluginDefaults,
            InputContextLayer::User => KeymapLayer::Mode,
        };
        let keymap = result
            .bindings
            .entry(context.clone())
            .or_default()
            .layer_mut(layer);
        if matches!(
            entry.operation,
            KeymapOperation::Unmap | KeymapOperation::Disable
        ) {
            keymap.insert_disabled(&sequence);
            result.inspections.retain(|inspection| {
                inspection.context != context || inspection.keys != effective_keys
            });
            result.inspections.push(KeymapInspection {
                id: entry.id.clone(),
                context,
                contribution_layer: entry.origin.layer,
                contribution_owner: entry.origin.owner.clone(),
                plugin_load: entry.origin.plugin_load,
                operation: entry.operation,
                command: None,
                arguments: BTreeMap::new(),
                source_keys: entry.keys.clone(),
                keys: effective_keys,
                desc: None,
                source,
            });
            continue;
        }
        if entry.operation == KeymapOperation::PassThrough {
            keymap.insert_pass_through(&sequence);
            result.inspections.push(KeymapInspection {
                id: entry.id.clone(),
                context,
                contribution_layer: entry.origin.layer,
                contribution_owner: entry.origin.owner.clone(),
                plugin_load: entry.origin.plugin_load,
                operation: entry.operation,
                command: None,
                arguments: BTreeMap::new(),
                source_keys: entry.keys.clone(),
                keys: effective_keys,
                desc: None,
                source,
            });
            continue;
        }
        let command_id = entry
            .command
            .as_deref()
            .expect("identity resolver guarantees command operation");
        let mut invocation = match CommandInvocation::new(command_id, InvocationSource::Keymap) {
            Ok(invocation) => invocation,
            Err(error) => {
                diagnostics.push(format!("keymap id `{}`: {error}", entry.id));
                continue;
            }
        };
        let mut valid_arguments = true;
        for (name, value) in &entry.arguments {
            let value = match value {
                toml::Value::String(value) => CommandValue::String(value.clone()),
                toml::Value::Boolean(value) => CommandValue::Bool(*value),
                toml::Value::Integer(value) => CommandValue::Integer(*value),
                _ => {
                    diagnostics.push(format!(
                        "keymap id `{}` argument `{name}` must be string, boolean, or integer",
                        entry.id
                    ));
                    valid_arguments = false;
                    continue;
                }
            };
            invocation.arguments.insert(name.clone(), value);
        }
        if !valid_arguments {
            continue;
        }
        if let Err(error) = commands.validate_invocation_shape(&invocation) {
            diagnostics.push(format!("keymap id `{}`: {error}", entry.id));
            continue;
        }
        result.inspections.push(KeymapInspection {
            id: entry.id.clone(),
            context: context.clone(),
            contribution_layer: entry.origin.layer,
            contribution_owner: entry.origin.owner.clone(),
            plugin_load: entry.origin.plugin_load,
            operation: KeymapOperation::Command,
            command: Some(invocation.id.as_str().into()),
            arguments: invocation.arguments.clone(),
            source_keys: entry.keys.clone(),
            keys: effective_keys.clone(),
            desc: entry.desc.clone(),
            source,
        });
        keymap.insert(&sequence, invocation);
    }

    if let Ok(effective_groups) = resolve_key_group_identities(groups, group_origins)
        .inspect_err(|errors| diagnostics.extend(errors.iter().cloned()))
    {
        let mut group_prefixes = HashMap::<(String, Vec<KeyEvent>), String>::new();
        for group in effective_groups {
            let context = match configured_context(contexts, &group.context, &group.mode) {
                Ok(context) => context,
                Err(error) => {
                    diagnostics.push(format!(
                        "key group id `{}` from declaration {}: {error}",
                        group.id, group.origin.declaration
                    ));
                    continue;
                }
            };
            let sequence = match expanded_key_sequence(&group.prefix, &leader, &local_leader) {
                Ok(sequence) => sequence,
                Err(error) => {
                    diagnostics.push(format!(
                        "key group id `{}` from declaration {}: {error}",
                        group.id, group.origin.declaration
                    ));
                    continue;
                }
            };
            let effective_prefix = display_key_sequence(&sequence);
            if let Some(previous) =
                group_prefixes.insert((context.clone(), sequence.clone()), group.id.clone())
            {
                diagnostics.push(format!(
                    "key group id `{}` prefix {} duplicates id `{previous}` in `{context}`",
                    group.id,
                    display_key_declaration(&group.prefix, &effective_prefix)
                ));
                continue;
            }
            let reachable = result.inspections.iter().any(|inspection| {
                if inspection.context != context || inspection.command.is_none() {
                    return false;
                }
                inspection
                    .keys
                    .split_whitespace()
                    .map(KeyEvent::from_str)
                    .collect::<Result<Vec<_>, _>>()
                    .is_ok_and(|binding| {
                        binding.len() > sequence.len() && binding.starts_with(&sequence)
                    })
            });
            if !reachable {
                diagnostics.push(format!(
                    "key group id `{}` prefix {} reaches no resolved binding in `{context}`",
                    group.id,
                    display_key_declaration(&group.prefix, &effective_prefix)
                ));
                continue;
            }
            let source = group.origin.source.clone();
            result.groups.push(ResolvedKeyGroup {
                id: group.id,
                context,
                contribution_layer: group.origin.layer,
                contribution_owner: group.origin.owner,
                plugin_load: group.origin.plugin_load,
                source_prefix: group.prefix,
                prefix: effective_prefix,
                label: group.label,
                icon: group.icon,
                order: group.order,
                source,
            });
        }
        result.groups.sort_by(|left, right| {
            left.context
                .cmp(&right.context)
                .then_with(|| left.order.cmp(&right.order))
                .then_with(|| left.prefix.cmp(&right.prefix))
                .then_with(|| left.id.cmp(&right.id))
        });
    }
    // Replay the actual layered resolver: an exact command or disabled ancestor can
    // make a declaration unreachable even when its own sequence was not overridden.
    result.inspections.retain(|inspection| {
        let sequence = inspection
            .keys
            .split_whitespace()
            .map(|key| KeyEvent::from_str(key).expect("compiled key sequence"))
            .collect::<Vec<_>>();
        let bindings = &result.bindings[&inspection.context];
        let mut cursor = KeymapSequence::default();
        for (index, key) in sequence.iter().enumerate() {
            let mut resolution = cursor.feed(bindings, key.clone());
            if index + 1 < sequence.len() {
                if !matches!(resolution, MappingResolution::Pending | MappingResolution::Ambiguous) {
                    return false;
                }
                continue;
            }
            let Some(command) = &inspection.command else {
                return true;
            };
            if resolution == MappingResolution::Ambiguous {
                resolution = cursor.flush_timeout();
            }
            return matches!(resolution, MappingResolution::Ready(invocation)
                if invocation.id.as_str() == command && invocation.arguments == inspection.arguments);
        }
        false
    });
    result.groups.retain(|group| {
        result.inspections.iter().any(|inspection| {
            inspection.context == group.context
                && inspection.command.is_some()
                && inspection
                    .keys
                    .strip_prefix(&group.prefix)
                    .is_some_and(|suffix| suffix.starts_with(' '))
        })
    });
    // Choose presentation bindings only after all overrides and reachability checks,
    // so overriding the preferred shortcut can fall back to another working one.
    for inspection in &result.inspections {
        let Some(command) = &inspection.command else {
            continue;
        };
        let mut invocation =
            CommandInvocation::new(command, InvocationSource::Keymap).expect("validated command");
        invocation.arguments = inspection.arguments.clone();
        let key = (
            inspection.context.clone(),
            invocation_fingerprint(&invocation),
        );
        let replaces = result.reverse.get(&key).is_none_or(|existing| {
            inspection.contribution_layer > existing.layer
                || (inspection.contribution_layer == existing.layer
                    && inspection.keys.split_whitespace().count()
                        < existing.keys.split_whitespace().count())
        });
        if replaces {
            result.reverse.insert(
                key,
                ReverseBinding {
                    id: inspection.id.clone(),
                    keys: inspection.keys.clone(),
                    layer: inspection.contribution_layer,
                },
            );
        }
    }
    diagnostics.extend(key_conflicts.into_iter().map(|(conflict, contexts)| {
        match contexts.as_slice() {
            [only] => format!("{conflict} in `{only}`"),
            [first, rest @ ..] => {
                format!("{conflict} in `{first}` and {} other contexts", rest.len())
            }
            [] => conflict,
        }
    }));
    if diagnostics.is_empty() {
        Ok(result)
    } else {
        Err(diagnostics)
    }
}
