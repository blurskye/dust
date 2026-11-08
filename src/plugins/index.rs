//! Validated plugin contributions and their owners.

use crate::plugins::component::ComponentHostState;
use crate::plugins::component::ComponentLimits;
use crate::plugins::component::component_api;
use crate::plugins::component::component_confirmation_result;
use crate::plugins::component::component_provider_request;
use crate::plugins::component::component_storage_result;
use crate::plugins::component::component_ui_error;
use crate::plugins::component::component_virtual_document_request;
use crate::plugins::execution::ActiveRuntime;
use crate::plugins::execution::LoadOutcome;
use crate::plugins::execution::LoadedComponent;
use crate::plugins::execution::PluginEdit;
use crate::plugins::execution::PluginEditProposal;
use crate::plugins::execution::PluginExecution;
use crate::plugins::execution::PluginExecutionRequest;
use crate::plugins::execution::PluginLoadId;
use crate::plugins::manifest::MAXIMUM_PLUGIN_EVENT_QUEUE;
use crate::plugins::manifest::MAXIMUM_PLUGIN_PROVIDERS;
use crate::plugins::manifest::MAXIMUM_PLUGIN_SUBSCRIPTIONS;
use crate::plugins::manifest::MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS;
use crate::plugins::manifest::MAXIMUM_PRIVATE_BYTES;
use crate::plugins::manifest::MAXIMUM_PRIVATE_CELLS_PER_CONTENT;
use crate::plugins::manifest::MAXIMUM_PRIVATE_CELLS_PER_PLUGIN;
use crate::plugins::manifest::MAXIMUM_PRIVATE_DIRTY_RECTANGLES;
use crate::plugins::manifest::MAXIMUM_PRIVATE_HIT_REGIONS;
use crate::plugins::manifest::MAXIMUM_PRIVATE_PATCH_CELLS;
use crate::plugins::manifest::MAXIMUM_PRIVATE_STYLES;
use crate::plugins::manifest::MAXIMUM_PROVIDER_BYTES;
use crate::plugins::manifest::MAXIMUM_PROVIDER_ITEMS;
use crate::plugins::manifest::MAXIMUM_STORAGE_ITEMS;
use crate::plugins::manifest::MAXIMUM_STORAGE_TOTAL_BYTES;
use crate::plugins::manifest::MAXIMUM_STORAGE_VALUE_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_DOCUMENT_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_DOCUMENT_LINES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_PATCH_BYTES;
use crate::plugins::manifest::PLUGIN_UI_HOST_COMMANDS;
use crate::plugins::manifest::PluginArgumentKind;
use crate::plugins::manifest::PluginCapability;
use crate::plugins::manifest::PluginContributionIndex;
use crate::plugins::manifest::PluginContributionProvenance;
use crate::plugins::manifest::PluginLimits;
use crate::plugins::manifest::PluginManifest;
use crate::plugins::manifest::PluginPresenterKind;
use crate::plugins::manifest::PluginProviderKind;
use crate::plugins::manifest::PluginSubscriptionKind;
use crate::plugins::manifest::PluginVirtualDocumentMode;
use crate::plugins::providers::PROVIDER_REQUEST_QUEUE_LIMIT;
use crate::plugins::storage::STORAGE_REQUEST_LIMIT;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_LANGUAGE_BYTE_LIMIT;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_TITLE_BYTE_LIMIT;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use thiserror::Error;

pub(crate) fn index_manifest_from_store(
    store: &Path,
    manifest_relative: &Path,
    component_relative: &Path,
    load: PluginLoadId,
    limits: &PluginLimits,
) -> Result<(PluginManifest, PluginContributionIndex, PathBuf, PathBuf), PluginError> {
    const MANIFEST_BYTE_LIMIT: u64 = 1024 * 1024;
    if manifest_relative.is_absolute() || component_relative.is_absolute() {
        return Err(PluginError::OutsideStore);
    }
    let store = crate::files::canonicalize(store).map_err(PluginError::Io)?;
    let manifest_path =
        crate::files::canonicalize(store.join(manifest_relative)).map_err(PluginError::Io)?;
    if !manifest_path.starts_with(&store) {
        return Err(PluginError::OutsideStore);
    }
    let file = crate::files::open_regular_file(&manifest_path).map_err(PluginError::Io)?;
    let metadata = file.metadata().map_err(PluginError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > MANIFEST_BYTE_LIMIT {
        return Err(PluginError::InvalidManifest);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MANIFEST_BYTE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(PluginError::Io)?;
    if bytes.len() as u64 > MANIFEST_BYTE_LIMIT {
        return Err(PluginError::InvalidManifest);
    }
    let source = String::from_utf8(bytes).map_err(|_| PluginError::InvalidManifest)?;
    let manifest: PluginManifest =
        toml::from_str(&source).map_err(|_| PluginError::InvalidManifest)?;
    validate_manifest(&manifest, limits)?;
    let index = index_manifest_contributions_with_text(
        &manifest,
        load.0,
        manifest_path.display().to_string(),
        Some(&source),
    )?;
    Ok((manifest, index, store, component_relative.to_path_buf()))
}

pub(super) fn load_manifest_component(
    store: &Path,
    manifest_relative: &Path,
    component_relative: &Path,
    limits: &PluginLimits,
) -> Result<LoadOutcome, (String, PluginError)> {
    let fallback = manifest_relative
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .chars()
        .take(256)
        .collect::<String>();
    let result = (|| {
        let (manifest, _, store, component_relative) = index_manifest_from_store(
            store,
            manifest_relative,
            component_relative,
            PluginLoadId(0),
            limits,
        )?;
        load_component(&store, &store.join(component_relative), manifest, limits)
    })();
    result.map_err(|error| (fallback, error))
}

pub(super) fn valid_plugin_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

pub(super) fn valid_contribution_id(value: &str) -> bool {
    value.len() <= 128
        && value.split('.').count() >= 3
        && value.split('.').all(|segment| {
            segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

pub(super) fn valid_contributed_sequence(source: &str) -> bool {
    use std::str::FromStr;
    let tokens = source.split_whitespace().collect::<Vec<_>>();
    !tokens.is_empty()
        && source.len() <= 1_024
        && tokens.len() <= 64
        && !source.chars().any(char::is_control)
        && tokens.iter().all(|token| {
            matches!(*token, "<leader>" | "<localleader>")
                || crate::editor::input::KeyEvent::from_str(token).is_ok()
        })
}

pub fn index_manifest_contributions(
    manifest: &PluginManifest,
    plugin_load: u64,
    source: String,
) -> Result<PluginContributionIndex, PluginError> {
    index_manifest_contributions_with_text(manifest, plugin_load, source, None)
}

pub(super) fn index_manifest_contributions_with_text(
    manifest: &PluginManifest,
    plugin_load: u64,
    source: String,
    source_text: Option<&str>,
) -> Result<PluginContributionIndex, PluginError> {
    if !valid_plugin_segment(&manifest.id)
        || manifest.version.is_empty()
        || manifest.version.len() > 128
        || manifest.version.chars().any(char::is_control)
        || manifest.component_sha256.len() != 64
        || !manifest
            .component_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || manifest.contributions.schema_version != 1
        || manifest.contributions.input_contexts.len() > 64
        || manifest.contributions.keymaps.len() > 256
        || manifest.contributions.key_groups.len() > 64
        || manifest.contributions.content_types.len() > 64
        || manifest.contributions.subscriptions.len() > MAXIMUM_PLUGIN_SUBSCRIPTIONS
        || manifest.contributions.providers.len() > MAXIMUM_PLUGIN_PROVIDERS
        || manifest.contributions.virtual_documents.len() > MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS
        || source.len() > 4_096
        || source.chars().any(char::is_control)
    {
        return Err(PluginError::InvalidManifest);
    }
    let namespace = format!("plugin.{}.", manifest.id);
    if manifest.commands.iter().any(|(id, declaration)| {
        !valid_plugin_segment(id)
            || crate::editor::CommandId::new(format!("{namespace}{id}")).is_err()
            || declaration.title.trim().is_empty()
            || declaration.title.len() > 256
            || declaration.title.chars().any(char::is_control)
            || declaration.description.len() > 2_048
            || declaration.description.chars().any(char::is_control)
            || declaration.arguments.len() > 32
            || declaration
                .arguments
                .iter()
                .any(|(name, _)| !valid_plugin_segment(name))
    }) {
        return Err(PluginError::InvalidManifest);
    }
    let mut contexts = crate::editor::input::InputContexts::bundled();
    let mut ids = BTreeSet::new();
    let mut indexed_contexts = manifest.contributions.input_contexts.clone();
    indexed_contexts.sort_by(|left, right| left.id.cmp(&right.id));
    for context in &indexed_contexts {
        let local = context
            .id
            .strip_prefix(&namespace)
            .ok_or(PluginError::InvalidManifest)?;
        if !ids.insert(context.id.clone()) {
            return Err(PluginError::InvalidManifest);
        }
        contexts
            .register_plugin(
                &manifest.id,
                local,
                &context.title,
                context.parent_surface.clone(),
                plugin_load,
                source.clone(),
            )
            .map_err(|_| PluginError::InvalidManifest)?;
    }
    let mut content_types = manifest.contributions.content_types.clone();
    content_types.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_private_cells = 0_usize;
    for (declaration, content) in content_types.iter_mut().enumerate() {
        content.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            plugin_load,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&content.id)
                    .map(|start| start..start.saturating_add(content.id.len()))
            }),
        });
        let declared_command = content
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        let presenter_valid = match (&content.presenter, &content.private_cells) {
            (PluginPresenterKind::RetainedDeclarative, None) => true,
            (PluginPresenterKind::PrivateCells, Some(policy)) => {
                aggregate_private_cells =
                    aggregate_private_cells.saturating_add(usize::from(policy.maximum_cells));
                manifest
                    .capabilities
                    .contains(&PluginCapability::PrivateCells)
                    && (1..=MAXIMUM_PRIVATE_CELLS_PER_CONTENT).contains(&policy.maximum_cells)
                    && (1..=MAXIMUM_PRIVATE_PATCH_CELLS).contains(&policy.maximum_patch_cells)
                    && policy.maximum_patch_cells <= policy.maximum_cells
                    && (1..=MAXIMUM_PRIVATE_DIRTY_RECTANGLES)
                        .contains(&policy.maximum_dirty_rectangles)
                    && (1..=MAXIMUM_PRIVATE_STYLES).contains(&policy.maximum_styles)
                    && (1..=MAXIMUM_PRIVATE_HIT_REGIONS).contains(&policy.maximum_hit_regions)
                    && (1..=MAXIMUM_PRIVATE_BYTES).contains(&policy.maximum_bytes)
                    && aggregate_private_cells <= MAXIMUM_PRIVATE_CELLS_PER_PLUGIN
            }
            _ => false,
        };
        if !content.id.starts_with(&namespace)
            || !valid_contribution_id(&content.id)
            || !presenter_valid
            || !ids.insert(content.id.clone())
            || content.title.trim().is_empty()
            || content.title.len() > 256
            || content.title.chars().any(char::is_control)
            || content.allowed_placements.is_empty()
            || content.allowed_placements.len() > 4
            || !content
                .allowed_placements
                .contains(&content.default_placement)
            || !(1..=16).contains(&content.maximum_instances)
            || contexts.resolve(&content.context, "").is_err()
            || declared_command.is_none()
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    let mut subscriptions = manifest.contributions.subscriptions.clone();
    subscriptions.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_event_queue = 0_usize;
    for (declaration, subscription) in subscriptions.iter_mut().enumerate() {
        subscription.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            plugin_load,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&subscription.id)
                    .map(|start| start..start.saturating_add(subscription.id.len()))
            }),
        });
        aggregate_event_queue = aggregate_event_queue
            .checked_add(usize::from(subscription.maximum_queued))
            .ok_or(PluginError::InvalidManifest)?;
        if !manifest
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
            || !subscription.id.starts_with(&namespace)
            || !valid_contribution_id(&subscription.id)
            || !ids.insert(subscription.id.clone())
            || !(1..=64).contains(&subscription.maximum_queued)
            || !(1..=120).contains(&subscription.maximum_rate_per_second)
            || !subscription.coalesce
            || aggregate_event_queue > MAXIMUM_PLUGIN_EVENT_QUEUE
        {
            return Err(PluginError::InvalidManifest);
        }
    }

    let mut providers = manifest.contributions.providers.clone();
    providers.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_provider_bytes = 0_u64;
    for (declaration, provider) in providers.iter_mut().enumerate() {
        provider.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            plugin_load,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&provider.id)
                    .map(|start| start..start.saturating_add(provider.id.len()))
            }),
        });
        let command = provider
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        aggregate_provider_bytes = aggregate_provider_bytes
            .checked_add(u64::from(provider.maximum_bytes))
            .ok_or(PluginError::InvalidManifest)?;
        if provider.kind != PluginProviderKind::PickerSource
            || !manifest
                .capabilities
                .contains(&PluginCapability::PickerProviders)
            || !provider.id.starts_with(&namespace)
            || !valid_contribution_id(&provider.id)
            || !ids.insert(provider.id.clone())
            || command
                .is_none_or(|command| command.arguments.values().any(|argument| argument.required))
            || !(1..=8).contains(&provider.maximum_concurrent_requests)
            || !(1..=MAXIMUM_PROVIDER_ITEMS).contains(&provider.maximum_items)
            || !(1..=MAXIMUM_PROVIDER_BYTES).contains(&provider.maximum_bytes)
            || !(10..=30_000).contains(&provider.deadline_milliseconds)
            || aggregate_provider_bytes > u64::from(MAXIMUM_PROVIDER_BYTES) * 4
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::PickerProviders)
        == providers.is_empty()
    {
        return Err(PluginError::InvalidManifest);
    }

    let mut virtual_documents = manifest.contributions.virtual_documents.clone();
    virtual_documents.sort_by(|left, right| left.id.cmp(&right.id));
    let mut aggregate_virtual_bytes = 0_u64;
    for (declaration, document) in virtual_documents.iter_mut().enumerate() {
        document.provenance = Some(PluginContributionProvenance {
            owner: manifest.id.clone(),
            plugin_load,
            source: source.clone(),
            declaration,
            span: source_text.and_then(|text| {
                text.find(&document.id)
                    .map(|start| start..start.saturating_add(document.id.len()))
            }),
        });
        let command = document
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        aggregate_virtual_bytes = aggregate_virtual_bytes
            .checked_add(u64::from(document.maximum_document_bytes))
            .ok_or(PluginError::InvalidManifest)?;
        if !manifest
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
            || !document.id.starts_with(&namespace)
            || !valid_contribution_id(&document.id)
            || !ids.insert(document.id.clone())
            || document.title.trim().is_empty()
            || document.title.len() > VIRTUAL_DOCUMENT_TITLE_BYTE_LIMIT
            || document.title.chars().any(char::is_control)
            || document.language.is_empty()
            || document.language.len() > VIRTUAL_DOCUMENT_LANGUAGE_BYTE_LIMIT
            || !document
                .language
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || document.mode != PluginVirtualDocumentMode::ReadOnly
            || !(1..=16).contains(&document.maximum_concurrent_documents)
            || !(1..=MAXIMUM_VIRTUAL_DOCUMENT_BYTES).contains(&document.maximum_document_bytes)
            || !(1..=MAXIMUM_VIRTUAL_DOCUMENT_LINES).contains(&document.maximum_document_lines)
            || !(1..=MAXIMUM_VIRTUAL_PATCH_BYTES).contains(&document.maximum_patch_bytes)
            || document.maximum_patch_bytes > document.maximum_document_bytes
            || !(1..=256).contains(&document.maximum_patches_per_request)
            || !(10..=30_000).contains(&document.deadline_milliseconds)
            || command
                .is_none_or(|command| command.arguments.values().any(|argument| argument.required))
            || contexts.resolve(&document.context, "").is_err()
            || aggregate_virtual_bytes > MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::VirtualDocuments)
        == virtual_documents.is_empty()
    {
        return Err(PluginError::InvalidManifest);
    }

    let storage = manifest.contributions.storage.clone();
    let storage_valid = storage.as_ref().is_some_and(|policy| {
        manifest
            .capabilities
            .contains(&PluginCapability::PluginStorage)
            && (1..=MAXIMUM_STORAGE_ITEMS).contains(&policy.maximum_items)
            && (1..=MAXIMUM_STORAGE_VALUE_BYTES).contains(&policy.maximum_value_bytes)
            && policy.maximum_value_bytes <= policy.maximum_total_bytes
            && (1..=MAXIMUM_STORAGE_TOTAL_BYTES).contains(&policy.maximum_total_bytes)
    });
    if storage.is_some() != storage_valid
        || manifest
            .capabilities
            .contains(&PluginCapability::PluginStorage)
            != storage.is_some()
    {
        return Err(PluginError::InvalidManifest);
    }

    let mut keymaps = manifest.contributions.keymaps.clone();
    keymaps.sort_by(|left, right| left.id.cmp(&right.id));
    let mut map_sequences = BTreeSet::new();
    for keymap in &keymaps {
        let argument_bytes = keymap
            .arguments
            .iter()
            .try_fold(0usize, |total, (name, value)| {
                let value_bytes = match value {
                    toml::Value::String(value) if value.len() <= 1_024 => value.len(),
                    toml::Value::Integer(_) | toml::Value::Boolean(_) => 16,
                    _ => return None,
                };
                total.checked_add(name.len())?.checked_add(value_bytes)
            });
        let declared_command = keymap
            .command
            .strip_prefix(&namespace)
            .and_then(|command| manifest.commands.get(command));
        let retained_host_command = PLUGIN_UI_HOST_COMMANDS.contains(&keymap.command.as_str());
        let arguments_match = (retained_host_command && keymap.arguments.is_empty())
            || declared_command.is_some_and(|command| {
                keymap.arguments.iter().all(|(name, value)| {
                    command.arguments.get(name).is_some_and(|field| {
                        matches!(
                            (field.kind, value),
                            (PluginArgumentKind::String, toml::Value::String(_))
                                | (PluginArgumentKind::Boolean, toml::Value::Boolean(_))
                                | (PluginArgumentKind::Integer, toml::Value::Integer(_))
                        )
                    })
                }) && command
                    .arguments
                    .iter()
                    .all(|(name, field)| !field.required || keymap.arguments.contains_key(name))
            });
        if !keymap.id.starts_with(&namespace)
            || !valid_contribution_id(&keymap.id)
            || !ids.insert(keymap.id.clone())
            || !arguments_match
            || !valid_contributed_sequence(&keymap.keys)
            || keymap.arguments.len() > 32
            || argument_bytes.is_none_or(|bytes| bytes > 4_096)
            || contexts.resolve(&keymap.context, &keymap.mode).is_err()
            || !map_sequences.insert((
                keymap.context.clone(),
                keymap.mode.clone(),
                keymap.keys.clone(),
            ))
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    let mut key_groups = manifest.contributions.key_groups.clone();
    key_groups.sort_by(|left, right| left.id.cmp(&right.id));
    let mut group_prefixes = BTreeSet::new();
    for group in &key_groups {
        if !group.id.starts_with(&namespace)
            || !valid_contribution_id(&group.id)
            || !ids.insert(group.id.clone())
            || !valid_contributed_sequence(&group.prefix)
            || group.label.is_empty()
            || group.label.len() > 256
            || group.label.chars().any(char::is_control)
            || group.icon.as_ref().is_some_and(|icon| {
                icon.is_empty()
                    || icon.len() > 32
                    || icon.chars().count() > 8
                    || icon.chars().any(char::is_control)
                    || unicode_width::UnicodeWidthStr::width(icon.as_str()) == 0
            })
            || contexts.resolve(&group.context, &group.mode).is_err()
            || !group_prefixes.insert((
                group.context.clone(),
                group.mode.clone(),
                group.prefix.clone(),
            ))
        {
            return Err(PluginError::InvalidManifest);
        }
    }
    Ok(PluginContributionIndex {
        capabilities: manifest.capabilities.clone(),
        owner: manifest.id.clone(),
        version: manifest.version.clone(),
        checksum: manifest.component_sha256.clone(),
        plugin_load,
        source,
        commands: manifest.commands.clone(),
        contexts: indexed_contexts,
        keymaps,
        key_groups,
        content_types,
        subscriptions,
        providers,
        virtual_documents,
        storage,
    })
}

pub(super) fn validate_manifest(
    manifest: &PluginManifest,
    limits: &PluginLimits,
) -> Result<(), PluginError> {
    if manifest.id.is_empty()
        || manifest.id.len() > 256
        || manifest.version.is_empty()
        || manifest.component_sha256.len() != 64
        || manifest.commands.len() > limits.maximum_commands
        || manifest
            .commands
            .keys()
            .any(|command| command.is_empty() || command.len() > 256)
    {
        return Err(PluginError::InvalidManifest);
    }
    index_manifest_contributions(manifest, 0, "<manifest-validation>".into())?;
    Ok(())
}
pub fn activate_candidate(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<Vec<String>, PluginError> {
    validate_manifest(&manifest, limits)?;
    let outcome = load_component(store, path, manifest, limits)?;
    Ok(outcome.loaded.activated_commands)
}

pub(super) struct ComponentDeadline {
    cancel: std::sync::mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl ComponentDeadline {
    pub(super) fn arm(engine: &wasmtime::Engine, ticks: u64) -> Result<Self, PluginError> {
        let engine = engine.clone();
        let (cancel, receiver) = std::sync::mpsc::channel();
        let duration = Duration::from_millis(ticks.max(1).saturating_mul(10));
        let worker = thread::Builder::new()
            .name("dust-plugin-deadline".into())
            .spawn(move || {
                if matches!(
                    receiver.recv_timeout(duration),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    engine.increment_epoch();
                }
            })
            .map_err(PluginError::Io)?;
        Ok(Self {
            cancel,
            worker: Some(worker),
        })
    }
}
impl Drop for ComponentDeadline {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn load_component(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<LoadOutcome, PluginError> {
    let store = crate::files::canonicalize(store).map_err(PluginError::Io)?;
    let path = crate::files::canonicalize(path).map_err(PluginError::Io)?;
    if !path.starts_with(&store) {
        return Err(PluginError::OutsideStore);
    }
    let metadata = fs::symlink_metadata(&path).map_err(PluginError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > limits.maximum_component_bytes as u64 {
        return Err(PluginError::InvalidComponent);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&path)
        .map_err(PluginError::Io)?
        .take(limits.maximum_component_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PluginError::Io)?;
    if bytes.len() > limits.maximum_component_bytes || !bytes.starts_with(b"\0asm") {
        return Err(PluginError::InvalidComponent);
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != manifest.component_sha256.to_ascii_lowercase() {
        return Err(PluginError::ChecksumMismatch);
    }
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let component = wasmtime::component::Component::from_binary(&engine, &bytes)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let store_limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(limits.maximum_memory_bytes)
        .table_elements(limits.maximum_table_elements as usize)
        .instances(limits.maximum_instances as usize)
        .memories(limits.maximum_instances as usize)
        .tables(limits.maximum_instances as usize)
        .build();
    let host = ComponentHostState {
        editor_requests: Default::default(),
        runtime_epoch: 0,
        limits: ComponentLimits::new(store_limits, limits.maximum_memory_bytes),
        capabilities: manifest.capabilities.clone(),
        declared_content_types: manifest
            .contributions
            .content_types
            .iter()
            .map(|content| content.id.clone())
            .collect(),
        snapshot: None,
        proposals: Vec::new(),
        messages: Vec::new(),
        config: BTreeMap::new(),
        ui_revision: None,
        confirmation_available: false,
        confirmation_result_only: false,
        ui_requests: Vec::new(),
        ui_events: Vec::new(),
        next_ui_token: 1,
        pending_ui_tokens: BTreeSet::new(),
        confirmation_requests: Vec::new(),
        confirmation_results: Vec::new(),
        next_confirmation_token: 1,
        host_events: Vec::new(),
        delivered_host_event_ids: BTreeSet::new(),
        host_event_acknowledgements: Vec::new(),
        execution_available: false,
        pending_storage: BTreeMap::new(),
        storage_policy: manifest.contributions.storage.clone(),
        storage_requests: Vec::new(),
        storage_results: Vec::new(),
        next_storage_token: 1,
        provider_requests: Vec::new(),
        delivered_provider_requests: BTreeSet::new(),
        provider_chunks: Vec::new(),
        virtual_document_requests: Vec::new(),
        delivered_virtual_document_requests: BTreeSet::new(),
        virtual_document_proposals: Vec::new(),
    };
    let mut store = wasmtime::Store::new(&engine, host);
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.fuel)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    store.set_epoch_deadline(1);
    let mut linker = wasmtime::component::Linker::new(&engine);
    let deadline = ComponentDeadline::arm(&engine, limits.epoch_deadline_ticks)?;
    let activation = (|| {
        component_api::EditorPlugin::add_to_linker(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|error| format!("{error:#}"))?;
        let bindings = component_api::EditorPlugin::new(&mut store, &instance)
            .map_err(|error| format!("{error:#}"))?;
        // Lazy lifting keeps aliased guest strings from being copied before limits apply.
        use wasmtime::component::WasmList;
        use wasmtime::component::WasmStr;
        let activate = instance
            .get_typed_func::<(), (Result<WasmList<WasmStr>, WasmStr>,)>(&mut store, "activate")
            .map_err(|error| format!("{error:#}"))?;
        let (result,) = activate
            .call(&mut store, ())
            .map_err(|error| format!("{error:#}"))?;
        let commands = match result {
            Ok(list) => {
                if list.len() > limits.maximum_commands {
                    return Err("activation exceeds command count limit".into());
                }
                let mut commands = Vec::with_capacity(list.len());
                for index in 0..list.len() {
                    let command = list
                        .get(&mut store, index)
                        .unwrap()
                        .map_err(|error| format!("{error:#}"))?;
                    let command = command
                        .to_str(&store)
                        .map_err(|error| format!("{error:#}"))?;
                    if command.len() > 256 {
                        return Err("activation command exceeds byte limit".into());
                    }
                    commands.push(command.into_owned());
                }
                commands
            }
            Err(error) => {
                let error = error.to_str(&store).map_err(|error| format!("{error:#}"))?;
                return Err(error.chars().take(4096).collect());
            }
        };
        activate
            .post_return(&mut store)
            .map_err(|error| format!("{error:#}"))?;
        Ok::<_, String>((bindings, commands))
    })();
    drop(deadline);
    let (bindings, activated) = activation.map_err(PluginError::Activation)?;
    let activated_commands = activated;
    if activated_commands
        .iter()
        .any(|command| !manifest.commands.contains_key(command))
    {
        return Err(PluginError::UndeclaredCommand);
    }
    let commands = activated_commands.iter().cloned().collect();
    let loaded = LoadedComponent {
        canonical_path: path,
        bytes,
        manifest,
        instantiated: true,
        unavailable_host: None,
        activated_commands,
    };
    Ok(LoadOutcome {
        loaded,
        runtime: ActiveRuntime {
            store,
            bindings,
            commands,
            limits: limits.clone(),
        },
    })
}
pub(crate) fn execute_component(
    invocation: PluginExecutionRequest,
    runtime: &mut ActiveRuntime,
) -> Result<PluginExecution, PluginError> {
    let PluginExecutionRequest {
        mut call,
        delivery,
        runtime_epoch,
        plugin,
        command,
        arguments,
        snapshot,
        config,
        ui_revision,
        confirmation_available,
        ui_events,
        confirmation_results,
        host_events,
        storage_results,
        provider_requests,
        virtual_document_requests,
        request_id,
    } = invocation;
    if !runtime.commands.contains(&command) {
        return Err(PluginError::UndeclaredCommand);
    }
    runtime
        .store
        .set_fuel(runtime.limits.fuel)
        .map_err(|error| PluginError::Execution(error.to_string()))?;
    runtime.store.set_epoch_deadline(1);
    let started = std::time::Instant::now();
    let result_timeout =
        Duration::from_millis(runtime.limits.epoch_deadline_ticks.saturating_mul(10));
    // A late result is discarded without interrupting a reusable instance. The epoch
    // limit is separate: code still running after two seconds is a runaway.
    let deadline = ComponentDeadline::arm(
        runtime.store.engine(),
        runtime.limits.epoch_deadline_ticks.max(200),
    )?;
    {
        let state = runtime.store.data_mut();
        if state.runtime_epoch != runtime_epoch {
            state.runtime_epoch = runtime_epoch;
            state.storage_results.clear();
            state.pending_storage.clear();
            state.ui_events.clear();
            state.pending_ui_tokens.clear();
        }
        state.snapshot = snapshot.map(|snapshot| component_api::dust::plugin::host::Snapshot {
            buffer: snapshot.buffer,
            revision: snapshot.revision,
            utf8: snapshot.utf8,
        });
        state.config = config;
        state.ui_revision = ui_revision;
        state.confirmation_available = confirmation_available;
        state.confirmation_result_only = !confirmation_results.is_empty();
        state.execution_available = true;
        state.ui_events.extend(ui_events.into_iter().map(|event| {
            component_api::dust::plugin::host::UiEvent {
                token: event.token,
                handle: event.handle,
                graph_revision: event.graph_revision,
                tree_revision: event.tree_revision,
                surface_revision: event.surface_revision,
                geometry: event.geometry.map(|geometry| {
                    component_api::dust::plugin::host::SurfaceGeometry {
                        handle: geometry.handle,
                        width: geometry.width,
                        height: geometry.height,
                        dimensions_revision: geometry.dimensions_revision,
                    }
                }),
                action: event.action,
                error: event.error.map(component_ui_error),
            }
        }));
        let mut geometry = BTreeSet::new();
        let mut actions = 0;
        state.ui_events.reverse();
        state.ui_events.retain(|event| {
            if event.token != 0 {
                true
            } else if event.action.is_some() {
                actions += 1;
                actions <= 64
            } else {
                geometry.insert(event.handle)
            }
        });
        state.ui_events.reverse();
        state.confirmation_results = confirmation_results
            .into_iter()
            .take(64)
            .map(component_confirmation_result)
            .collect();
        state.confirmation_requests.clear();
        state.host_events = host_events
            .into_iter()
            .take(64)
            .map(|event| component_api::dust::plugin::host::HostEvent {
                id: event.id,
                sequence: event.sequence,
                kind: match event.kind {
                    PluginSubscriptionKind::WorkspaceMetadataChanged => {
                        component_api::dust::plugin::host::HostEventKind::WorkspaceMetadataChanged
                    }
                    PluginSubscriptionKind::ActiveSurfaceChanged => {
                        component_api::dust::plugin::host::HostEventKind::ActiveSurfaceChanged
                    }
                    PluginSubscriptionKind::BufferSummaryChanged => {
                        component_api::dust::plugin::host::HostEventKind::BufferSummaryChanged
                    }
                    PluginSubscriptionKind::PluginSurfaceChanged => {
                        component_api::dust::plugin::host::HostEventKind::PluginSurfaceChanged
                    }
                },
                revision: event.revision,
                payload: event.payload,
                coalesced: event.coalesced,
                dropped: event.dropped,
            })
            .collect();
        state.delivered_host_event_ids = state.host_events.iter().map(|event| event.id).collect();
        state.host_event_acknowledgements.clear();
        state.storage_results.extend(
            storage_results
                .into_iter()
                .take(STORAGE_REQUEST_LIMIT)
                .map(component_storage_result),
        );
        state.storage_requests.clear();
        state.provider_requests = provider_requests
            .into_iter()
            .take(PROVIDER_REQUEST_QUEUE_LIMIT)
            .map(component_provider_request)
            .collect();
        state.delivered_provider_requests = state
            .provider_requests
            .iter()
            .map(|request| request.id)
            .collect();
        state.provider_chunks.clear();
        state.virtual_document_requests = virtual_document_requests
            .into_iter()
            .take(VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT)
            .map(component_virtual_document_request)
            .collect();
        state.delivered_virtual_document_requests = state
            .virtual_document_requests
            .iter()
            .map(|request| request.request)
            .collect();
        state.virtual_document_proposals.clear();
        state
            .editor_requests
            .begin(delivery, call.pending, call.after);
        state.proposals.clear();
        state.messages.clear();
        state.ui_requests.clear();
    }
    if call.command.is_empty() {
        call.command = format!("plugin.{plugin}.{command}");
        call.arguments = crate::plugins::requests::positional_value(arguments);
    }
    let result = runtime
        .bindings
        .call_execute_command(&mut runtime.store, &command, &call);
    drop(deadline);
    // Drain effects even on traps and interruptions; failed executions publish none of them.
    let state = runtime.store.data_mut();
    let editor_requests = std::mem::take(&mut state.editor_requests.queued);
    let value = state.editor_requests.value.take();
    let call_next = state.editor_requests.call_next.take();
    let messages = std::mem::take(&mut state.messages);
    let ui_requests = std::mem::take(&mut state.ui_requests);
    let confirmation_requests = std::mem::take(&mut state.confirmation_requests);
    let host_event_acknowledgements = std::mem::take(&mut state.host_event_acknowledgements);
    let storage_requests = std::mem::take(&mut state.storage_requests);
    let provider_chunks = std::mem::take(&mut state.provider_chunks);
    let virtual_document_proposals = std::mem::take(&mut state.virtual_document_proposals);
    let proposals = std::mem::take(&mut state.proposals)
        .into_iter()
        .map(|(buffer, revision, edits)| PluginEditProposal {
            buffer,
            revision,
            edits: edits
                .into_iter()
                .map(|edit| PluginEdit {
                    start: edit.start,
                    end: edit.end,
                    replacement: edit.replacement,
                })
                .collect(),
        })
        .collect();
    let result = result
        .map_err(|error| PluginError::Execution(format!("{error:#}")))
        .and_then(|result| {
            if started.elapsed() > result_timeout {
                Err(PluginError::Deadline)
            } else if result
                .as_ref()
                .is_ok_and(|result| result.len() > 1024 * 1024)
            {
                Err(PluginError::InvalidInvocation)
            } else {
                Ok(result)
            }
        });
    if !matches!(&result, Ok(Ok(_))) {
        for request in &ui_requests {
            state.pending_ui_tokens.remove(&request.token);
        }
        for request in &storage_requests {
            state.pending_storage.remove(&request.token);
        }
    }
    let (result, error) = match result? {
        Ok(result) => (result, None),
        Err(error) => (String::new(), Some(error.chars().take(4096).collect())),
    };
    if error.is_some() {
        return Ok(PluginExecution {
            error,
            plugin,
            command,
            request_id,
            result,
            editor_requests: Vec::new(),
            value: None,
            call_next: None,
            messages: Vec::new(),
            proposals: Vec::new(),
            ui_requests: Vec::new(),
            confirmation_requests: Vec::new(),
            host_event_acknowledgements: Vec::new(),
            storage_requests: Vec::new(),
            provider_chunks: Vec::new(),
            virtual_document_proposals: Vec::new(),
        });
    }
    Ok(PluginExecution {
        error: None,
        editor_requests,
        value,
        call_next,
        plugin,
        command,
        request_id,
        result,
        messages,
        proposals,
        ui_requests,
        confirmation_requests,
        host_event_acknowledgements,
        storage_requests,
        provider_chunks,
        virtual_document_proposals,
    })
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("invalid plugin manifest")]
    InvalidManifest,
    #[error("invalid or oversized WebAssembly component")]
    InvalidComponent,
    #[error("component is outside immutable package store")]
    OutsideStore,
    #[error("component checksum does not match its manifest")]
    ChecksumMismatch,
    #[error("component validation failed: {0}")]
    Validation(String),
    #[error("plugin activation failed: {0}")]
    Activation(String),
    #[error("plugin execution failed: {0}")]
    Execution(String),
    #[error("plugin command failed: {0}")]
    CommandFailed(String),
    #[error("plugin command deadline elapsed; result discarded")]
    Deadline,
    #[error("plugin command was not declared")]
    UndeclaredCommand,
    #[error("plugin contribution conflicts with an installed plugin default")]
    ContributionConflict,
    #[error("plugin contribution index was not accepted for activation")]
    IndexNotAccepted,
    #[error("plugin invocation exceeded a bound")]
    InvalidInvocation,
    #[error("plugin already has a current load")]
    AlreadyActive,
    #[error("plugin active-load limit reached")]
    LoadLimit,
    #[error("plugin is not active")]
    NotActive,
    #[error("plugin loader queue is full")]
    QueueFull,
    #[error("plugin loader stopped")]
    Stopped,
    #[error("plugin I/O failed: {0}")]
    Io(#[source] std::io::Error),
}
