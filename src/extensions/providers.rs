#![forbid(unsafe_code)]

use super::{
    PluginContributionIndex, PluginLoadId, PluginProviderContribution, PluginProviderKind,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub const PROVIDER_REQUEST_QUEUE_CEILING: usize = 64;
pub const PROVIDER_CHUNK_ITEM_CEILING: usize = 512;
pub const PROVIDER_ITEM_ID_BYTE_CEILING: usize = 128;
pub const PROVIDER_PRIMARY_BYTE_CEILING: usize = 1_024;
pub const PROVIDER_SECONDARY_BYTE_CEILING: usize = 2_048;
pub const PROVIDER_ACTION_BYTE_CEILING: usize = 4_096;
pub const PROVIDER_QUERY_BYTE_CEILING: usize = 1_024;
pub const PROVIDER_CONTEXT_BYTE_CEILING: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginProviderErrorCode {
    QueueFull,
    UnknownRequest,
    UnknownProvider,
    StaleGeneration,
    StaleRevision,
    Cancelled,
    Expired,
    InvalidSequence,
    DuplicateItem,
    ItemCeiling,
    ByteCeiling,
    Malformed,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginProviderError {
    pub code: PluginProviderErrorCode,
    pub detail: String,
}

impl PluginProviderError {
    pub fn new(code: PluginProviderErrorCode, detail: impl std::fmt::Display) -> Self {
        Self {
            code,
            detail: detail
                .to_string()
                .chars()
                .filter(|character| !character.is_control())
                .take(1_024)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPickerRequestEnvelope {
    pub id: u64,
    pub provider: String,
    pub query: String,
    pub context: String,
    pub workspace_revision: Option<u64>,
    pub buffer_revision: Option<u64>,
    pub deadline_milliseconds: u32,
    pub cancellation_generation: u64,
    pub maximum_items: u16,
    pub maximum_bytes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPickerItem {
    pub id: String,
    pub primary: String,
    pub secondary: Option<String>,
    pub action_data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginProviderTerminal {
    More,
    Complete,
    Truncated,
    Error(PluginProviderError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPickerChunk {
    pub request: u64,
    pub sequence: u64,
    pub items: Vec<PluginPickerItem>,
    pub terminal: PluginProviderTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginProviderSessionStatus {
    Active,
    Complete,
    Truncated,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPickerProjection {
    pub plugin: String,
    pub load: PluginLoadId,
    pub provider: String,
    pub request: u64,
    pub action_command: String,
    pub query: String,
    pub items: Vec<PluginPickerItem>,
    pub status: PluginProviderSessionStatus,
    pub revision: u64,
}

#[derive(Clone, Debug)]
struct ProviderOwner {
    declaration: PluginProviderContribution,
    action_command: String,
}

#[derive(Clone, Debug)]
struct ProviderSession {
    plugin: String,
    load: PluginLoadId,
    provider: String,
    action_command: String,
    query: String,
    context: String,
    workspace_revision: Option<u64>,
    buffer_revision: Option<u64>,
    deadline: Instant,
    deadline_milliseconds: u32,
    cancellation_generation: u64,
    maximum_items: u16,
    maximum_bytes: u32,
    expected_sequence: u64,
    items: BTreeMap<String, PluginPickerItem>,
    ranked: Vec<String>,
    status: PluginProviderSessionStatus,
    revision: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PluginProviderBroker {
    owners: BTreeMap<(String, PluginLoadId, String), ProviderOwner>,
    sessions: BTreeMap<u64, ProviderSession>,
    next_request: u64,
    next_cancellation: u64,
}

pub struct PluginProviderStart {
    pub plugin: String,
    pub load: PluginLoadId,
    pub provider: String,
    pub query: String,
    pub context: String,
    pub workspace_revision: Option<u64>,
    pub buffer_revision: Option<u64>,
}

impl PluginProviderBroker {
    pub fn install(&mut self, index: &PluginContributionIndex) -> Result<(), PluginProviderError> {
        for declaration in &index.providers {
            if declaration.kind != PluginProviderKind::PickerSource {
                return Err(PluginProviderError::new(
                    PluginProviderErrorCode::UnknownProvider,
                    "provider kind has no runtime contract",
                ));
            }
            let action_command = declaration.command.clone();
            let key = (
                index.owner.clone(),
                PluginLoadId(index.generation),
                declaration.id.clone(),
            );
            if self
                .owners
                .insert(
                    key,
                    ProviderOwner {
                        declaration: declaration.clone(),
                        action_command,
                    },
                )
                .is_some()
            {
                return Err(PluginProviderError::new(
                    PluginProviderErrorCode::UnknownProvider,
                    "provider generation is already installed",
                ));
            }
        }
        Ok(())
    }

    pub fn start(&mut self, request: PluginProviderStart) -> Result<u64, PluginProviderError> {
        self.reserve_replacing(request, None)
    }
    // Reserve replacement metadata without cancelling the displayed request.
    // The owner cancels either reservation after picker admission, before execution.
    pub(crate) fn reserve_replacing(
        &mut self,
        request: PluginProviderStart,
        retiring: Option<u64>,
    ) -> Result<u64, PluginProviderError> {
        let PluginProviderStart {
            plugin,
            load,
            provider,
            query,
            context,
            workspace_revision,
            buffer_revision,
        } = request;
        validate_text(&query, PROVIDER_QUERY_BYTE_CEILING, true)?;
        validate_text(&context, PROVIDER_CONTEXT_BYTE_CEILING, true)?;
        let owner = self
            .owners
            .get(&(plugin.clone(), load, provider.clone()))
            .ok_or_else(|| {
                PluginProviderError::new(
                    PluginProviderErrorCode::UnknownProvider,
                    "picker provider is not owned by this generation",
                )
            })?
            .clone();
        let now = crate::clock::monotonic();
        let concurrent = self
            .sessions
            .iter()
            .filter(|(id, session)| {
                Some(**id) != retiring
                    && session.plugin == plugin
                    && session.load == load
                    && session.provider == provider
                    && session.status == PluginProviderSessionStatus::Active
                    && session.deadline >= now
            })
            .count();
        if concurrent >= usize::from(owner.declaration.maximum_concurrent_requests) {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::QueueFull,
                "picker provider concurrent request ceiling reached",
            ));
        }
        let request = self.next_request.max(1);
        self.next_request = request.checked_add(1).ok_or_else(|| {
            PluginProviderError::new(
                PluginProviderErrorCode::Internal,
                "provider request ID exhausted",
            )
        })?;
        let cancellation_generation = self.next_cancellation.max(1);
        self.next_cancellation = cancellation_generation.checked_add(1).ok_or_else(|| {
            PluginProviderError::new(
                PluginProviderErrorCode::Internal,
                "provider cancellation generation exhausted",
            )
        })?;
        let deadline_milliseconds = owner.declaration.deadline_milliseconds;
        self.sessions.insert(
            request,
            ProviderSession {
                plugin,
                load,
                provider,
                action_command: owner.action_command,
                query,
                context,
                workspace_revision,
                buffer_revision,
                deadline: now + Duration::from_millis(u64::from(deadline_milliseconds)),
                deadline_milliseconds,
                cancellation_generation,
                maximum_items: owner.declaration.maximum_items,
                maximum_bytes: owner.declaration.maximum_bytes,
                expected_sequence: 1,
                items: BTreeMap::new(),
                ranked: Vec::new(),
                status: PluginProviderSessionStatus::Active,
                revision: 1,
            },
        );
        Ok(request)
    }

    #[must_use]
    pub fn requests(&self, plugin: &str, load: PluginLoadId) -> Vec<PluginPickerRequestEnvelope> {
        let now = crate::clock::monotonic();
        self.sessions
            .iter()
            .filter(|(_, session)| {
                session.plugin == plugin
                    && session.load == load
                    && session.status == PluginProviderSessionStatus::Active
                    && session.deadline >= now
            })
            .take(PROVIDER_REQUEST_QUEUE_CEILING)
            .map(|(id, session)| PluginPickerRequestEnvelope {
                id: *id,
                provider: session.provider.clone(),
                query: session.query.clone(),
                context: session.context.clone(),
                workspace_revision: session.workspace_revision,
                buffer_revision: session.buffer_revision,
                deadline_milliseconds: session.deadline_milliseconds,
                cancellation_generation: session.cancellation_generation,
                maximum_items: session.maximum_items,
                maximum_bytes: session.maximum_bytes,
            })
            .collect()
    }

    pub fn apply_chunk(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        workspace_revision: Option<u64>,
        buffer_revision: Option<u64>,
        chunk: PluginPickerChunk,
    ) -> Result<PluginProviderSessionStatus, PluginProviderError> {
        let current = self.sessions.get(&chunk.request).ok_or_else(|| {
            PluginProviderError::new(
                PluginProviderErrorCode::UnknownRequest,
                "provider request is unknown",
            )
        })?;
        if current.plugin != plugin || current.load != load {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::StaleGeneration,
                "provider request belongs to another generation",
            ));
        }
        if current.status == PluginProviderSessionStatus::Cancelled {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::Cancelled,
                "provider request was cancelled",
            ));
        }
        if current.status != PluginProviderSessionStatus::Active {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::Malformed,
                "provider request already has a terminal state",
            ));
        }
        if crate::clock::monotonic() > current.deadline {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::Expired,
                "provider request deadline elapsed",
            ));
        }
        if current.workspace_revision != workspace_revision
            || current.buffer_revision != buffer_revision
        {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::StaleRevision,
                "provider context revision changed",
            ));
        }
        if chunk.sequence != current.expected_sequence {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::InvalidSequence,
                "provider chunk sequence is stale or has a gap",
            ));
        }
        validate_chunk(&chunk)?;
        let mut candidate = current.clone();
        let mut chunk_ids = BTreeSet::new();
        for item in chunk.items {
            if !chunk_ids.insert(item.id.clone()) || candidate.items.contains_key(&item.id) {
                return Err(PluginProviderError::new(
                    PluginProviderErrorCode::DuplicateItem,
                    "provider item ID is duplicated",
                ));
            }
            candidate.items.insert(item.id.clone(), item);
        }
        if candidate.items.len() > usize::from(candidate.maximum_items) {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::ItemCeiling,
                "provider item ceiling exceeded",
            ));
        }
        let aggregate = candidate.items.values().try_fold(0_usize, |total, item| {
            total
                .checked_add(item.id.len())?
                .checked_add(item.primary.len())?
                .checked_add(item.secondary.as_ref().map_or(0, String::len))?
                .checked_add(item.action_data.len())
        });
        if aggregate.is_none_or(|bytes| bytes > candidate.maximum_bytes as usize) {
            return Err(PluginProviderError::new(
                PluginProviderErrorCode::ByteCeiling,
                "provider aggregate byte ceiling exceeded",
            ));
        }
        candidate.expected_sequence =
            candidate.expected_sequence.checked_add(1).ok_or_else(|| {
                PluginProviderError::new(
                    PluginProviderErrorCode::InvalidSequence,
                    "provider sequence exhausted",
                )
            })?;
        candidate.status = match chunk.terminal {
            PluginProviderTerminal::More => PluginProviderSessionStatus::Active,
            PluginProviderTerminal::Complete => PluginProviderSessionStatus::Complete,
            PluginProviderTerminal::Truncated => PluginProviderSessionStatus::Truncated,
            PluginProviderTerminal::Error(_) => PluginProviderSessionStatus::Failed,
        };
        candidate.revision = candidate.revision.checked_add(1).ok_or_else(|| {
            PluginProviderError::new(
                PluginProviderErrorCode::Internal,
                "provider projection revision exhausted",
            )
        })?;
        candidate.ranked = rank(&candidate.query, &candidate.items);
        let status = candidate.status;
        self.sessions.insert(chunk.request, candidate);
        Ok(status)
    }

    pub(crate) fn continuation(&self, request: u64) -> Option<&str> {
        let session = self.sessions.get(&request)?;
        (session.status == PluginProviderSessionStatus::Active)
            .then_some(session.action_command.as_str())
    }
    pub(crate) fn presentable(&self, request: u64) -> bool {
        self.sessions
            .get(&request)
            .is_some_and(|session| session.status != PluginProviderSessionStatus::Cancelled)
    }
    pub(crate) fn owns_selection(
        &self,
        plugin: &str,
        load: PluginLoadId,
        request: u64,
        item: &str,
        command: &str,
    ) -> bool {
        self.sessions.get(&request).is_some_and(|session| {
            session.plugin == plugin
                && session.load == load
                && session.status != PluginProviderSessionStatus::Cancelled
                && session.action_command == command
                && session.items.contains_key(item)
        })
    }

    pub fn cancel(&mut self, request: u64) -> bool {
        let Some(session) = self.sessions.get_mut(&request) else {
            return false;
        };
        if session.status != PluginProviderSessionStatus::Cancelled {
            session.status = PluginProviderSessionStatus::Cancelled;
            session.cancellation_generation = session.cancellation_generation.saturating_add(1);
            session.revision = session.revision.saturating_add(1);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn projection(&self, request: u64) -> Option<PluginPickerProjection> {
        let session = self.sessions.get(&request)?;
        Some(PluginPickerProjection {
            plugin: session.plugin.clone(),
            load: session.load,
            provider: session.provider.clone(),
            request,
            action_command: session.action_command.clone(),
            query: session.query.clone(),
            items: session
                .ranked
                .iter()
                .filter_map(|id| session.items.get(id).cloned())
                .collect(),
            status: session.status,
            revision: session.revision,
        })
    }

    pub fn reset_runtime(&mut self) {
        self.sessions.clear();
    }

    pub fn remove_generation(&mut self, plugin: &str, load: PluginLoadId) {
        self.owners
            .retain(|(owner, generation, _), _| owner != plugin || *generation != load);
        self.sessions
            .retain(|_, session| session.plugin != plugin || session.load != load);
    }
}

fn validate_text(
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), PluginProviderError> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(PluginProviderError::new(
            PluginProviderErrorCode::Malformed,
            "provider text is empty, controlled, or oversized",
        ));
    }
    Ok(())
}

pub fn validate_chunk(chunk: &PluginPickerChunk) -> Result<(), PluginProviderError> {
    if chunk.request == 0 || chunk.sequence == 0 || chunk.items.len() > PROVIDER_CHUNK_ITEM_CEILING
    {
        return Err(PluginProviderError::new(
            PluginProviderErrorCode::ItemCeiling,
            "provider chunk count or identity is invalid",
        ));
    }
    let terminal_valid = match &chunk.terminal {
        PluginProviderTerminal::Error(error) => !error.detail.is_empty(),
        _ => true,
    };
    if !terminal_valid
        || chunk.items.iter().any(|item| {
            validate_text(&item.id, PROVIDER_ITEM_ID_BYTE_CEILING, false).is_err()
                || validate_text(&item.primary, PROVIDER_PRIMARY_BYTE_CEILING, false).is_err()
                || item.secondary.as_ref().is_some_and(|secondary| {
                    validate_text(secondary, PROVIDER_SECONDARY_BYTE_CEILING, true).is_err()
                })
                || item.action_data.len() > PROVIDER_ACTION_BYTE_CEILING
        })
    {
        return Err(PluginProviderError::new(
            PluginProviderErrorCode::Malformed,
            "provider chunk contains malformed data",
        ));
    }
    Ok(())
}

fn rank(query: &str, items: &BTreeMap<String, PluginPickerItem>) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    let mut ranked = items
        .values()
        .filter_map(|item| {
            let primary = item.primary.to_ascii_lowercase();
            let secondary = item.secondary.as_deref().unwrap_or("").to_ascii_lowercase();
            let position = if query.is_empty() {
                0
            } else {
                primary.find(&query).or_else(|| secondary.find(&query))?
            };
            Some((position, primary, item.id.clone()))
        })
        .collect::<Vec<_>>();
    ranked.sort();
    ranked.into_iter().map(|(_, _, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{
        PluginActivationBehavior, PluginCommandContext, PluginCommandDeclaration,
    };

    fn index(generation: u64, maximum_items: u16, maximum_bytes: u32) -> PluginContributionIndex {
        PluginContributionIndex {
            owner: "demo".into(),
            version: "1".into(),
            checksum: "0".repeat(64),
            generation,
            source: "test".into(),
            commands: [(
                "run".into(),
                PluginCommandDeclaration {
                    title: "Run".into(),
                    description: String::new(),
                    arguments: BTreeMap::new(),
                    context: PluginCommandContext::Application,
                    activation: PluginActivationBehavior::ActiveOnly,
                },
            )]
            .into_iter()
            .collect(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: vec![PluginProviderContribution {
                id: "plugin.demo.provider.files".into(),
                kind: PluginProviderKind::PickerSource,
                command: "plugin.demo.run".into(),
                maximum_concurrent_requests: 1,
                maximum_items,
                maximum_bytes,
                deadline_milliseconds: 30_000,
                provenance: None,
            }],
            virtual_documents: Vec::new(),
            storage: None,
        }
    }

    fn item(id: &str, primary: &str) -> PluginPickerItem {
        PluginPickerItem {
            id: id.into(),
            primary: primary.into(),
            secondary: None,
            action_data: Vec::new(),
        }
    }

    #[test]
    fn picker_chunks_are_revisioned_ranked_atomic_and_generation_owned() {
        let mut broker = PluginProviderBroker::default();
        broker.install(&index(7, 4, 4_096)).unwrap();
        let request = broker
            .start(PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(7),
                provider: "plugin.demo.provider.files".into(),
                query: "be".into(),
                context: "editor".into(),
                workspace_revision: Some(3),
                buffer_revision: Some(9),
            })
            .unwrap();
        assert_eq!(broker.requests("demo", PluginLoadId(7)).len(), 1);
        broker
            .apply_chunk(
                "demo",
                PluginLoadId(7),
                Some(3),
                Some(9),
                PluginPickerChunk {
                    request,
                    sequence: 1,
                    items: vec![item("z", "Zebra"), item("b", "Beta")],
                    terminal: PluginProviderTerminal::More,
                },
            )
            .unwrap();
        assert_eq!(broker.projection(request).unwrap().items[0].primary, "Beta");
        let stable = broker.clone();
        let rejected = broker.apply_chunk(
            "demo",
            PluginLoadId(7),
            Some(3),
            Some(9),
            PluginPickerChunk {
                request,
                sequence: 2,
                items: vec![item("b", "duplicate")],
                terminal: PluginProviderTerminal::Complete,
            },
        );
        assert_eq!(
            rejected.unwrap_err().code,
            PluginProviderErrorCode::DuplicateItem
        );
        assert_eq!(broker.projection(request), stable.projection(request));
        assert!(broker.cancel(request));
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(7),
                    Some(3),
                    Some(9),
                    PluginPickerChunk {
                        request,
                        sequence: 2,
                        items: Vec::new(),
                        terminal: PluginProviderTerminal::Complete,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::Cancelled
        );
        broker.remove_generation("demo", PluginLoadId(6));
        assert!(broker.projection(request).is_some());
        broker.remove_generation("demo", PluginLoadId(7));
        assert!(broker.projection(request).is_none());
    }

    #[test]
    fn hostile_sequences_revisions_ceilings_and_idle_are_bounded() {
        let mut broker = PluginProviderBroker::default();
        broker.install(&index(2, 1, 16)).unwrap();
        let request = broker
            .start(PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(2),
                provider: "plugin.demo.provider.files".into(),
                query: String::new(),
                context: String::new(),
                workspace_revision: Some(1),
                buffer_revision: None,
            })
            .unwrap();
        assert_eq!(
            broker
                .start(PluginProviderStart {
                    plugin: "demo".into(),
                    load: PluginLoadId(2),
                    provider: "plugin.demo.provider.files".into(),
                    query: String::new(),
                    context: String::new(),
                    workspace_revision: Some(1),
                    buffer_revision: None,
                })
                .unwrap_err()
                .code,
            PluginProviderErrorCode::QueueFull
        );
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(2),
                    Some(2),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 1,
                        items: Vec::new(),
                        terminal: PluginProviderTerminal::More,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::StaleRevision
        );
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(2),
                    Some(1),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 2,
                        items: Vec::new(),
                        terminal: PluginProviderTerminal::More,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::InvalidSequence
        );
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(3),
                    Some(1),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 1,
                        items: Vec::new(),
                        terminal: PluginProviderTerminal::More,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::StaleGeneration
        );
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(2),
                    Some(1),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 1,
                        items: vec![item("a", "a"), item("b", "b")],
                        terminal: PluginProviderTerminal::Complete,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::ItemCeiling
        );
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(2),
                    Some(1),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 1,
                        items: vec![item("long", "this exceeds sixteen")],
                        terminal: PluginProviderTerminal::Complete,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::ByteCeiling
        );
        broker.sessions.get_mut(&request).unwrap().deadline =
            crate::clock::monotonic() - Duration::from_millis(1);
        assert!(broker.requests("demo", PluginLoadId(2)).is_empty());
        assert_eq!(
            broker
                .apply_chunk(
                    "demo",
                    PluginLoadId(2),
                    Some(1),
                    None,
                    PluginPickerChunk {
                        request,
                        sequence: 1,
                        items: Vec::new(),
                        terminal: PluginProviderTerminal::More,
                    },
                )
                .unwrap_err()
                .code,
            PluginProviderErrorCode::Expired
        );
        assert!(
            broker
                .start(PluginProviderStart {
                    plugin: "demo".into(),
                    load: PluginLoadId(2),
                    provider: "plugin.demo.provider.files".into(),
                    query: String::new(),
                    context: String::new(),
                    workspace_revision: Some(1),
                    buffer_revision: None,
                })
                .is_ok()
        );
        let stable = broker.projection(request);
        for _ in 0..256 {
            assert_eq!(broker.projection(request), stable);
        }
    }
}
