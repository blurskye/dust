//! Typed, bounded AI-provider core.
//!
//! This module deliberately contains no editor, window, process, filesystem, or
//! network authority. Providers publish queued data; callers retain ownership of
//! editor identities and decide whether a revision-owned proposal is still valid.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

const MAX_ID_BYTES: usize = 128;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, AiError> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > MAX_ID_BYTES
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
                {
                    return Err(AiError::InvalidIdentifier);
                }
                Ok(Self(value))
            }
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(AiProviderId);
string_id!(AiModelId);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AiRequestId(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct AiStreamRevision(pub u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AiCapability {
    Chat,
    GhostText,
    Composer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretAvailability {
    Missing,
    Available,
}

#[derive(Clone, Eq, PartialEq)]
struct SecretHandle {
    logical_name: String,
    availability: SecretAvailability,
}

impl fmt::Debug for SecretHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretHandle")
            .field("logical_name", &"[REDACTED]")
            .field("availability", &self.availability)
            .finish()
    }
}

impl SecretHandle {
    fn new(logical_name: &str, availability: SecretAvailability) -> Result<Self, AiError> {
        if logical_name.is_empty()
            || logical_name.len() > MAX_ID_BYTES
            || !logical_name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(AiError::InvalidSecretName);
        }
        Ok(Self {
            logical_name: logical_name.to_owned(),
            availability,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiProviderKind {
    /// Deterministic provider used by tests and local host dogfood.
    FakeLocal,
    /// Reserved contract. No remote transport is implemented in this release.
    RemoteUnsupported,
}

#[derive(Clone, Eq, PartialEq)]
pub struct AiProviderConfig {
    id: AiProviderId,
    models: BTreeSet<AiModelId>,
    capabilities: BTreeSet<AiCapability>,
    kind: AiProviderKind,
    requires_network: bool,
    secret: Option<SecretHandle>,
}

impl fmt::Debug for AiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiProviderConfig")
            .field("id", &self.id)
            .field("models", &self.models)
            .field("capabilities", &self.capabilities)
            .field("kind", &self.kind)
            .field("requires_network", &self.requires_network)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl AiProviderConfig {
    pub fn fake_local(
        id: AiProviderId,
        models: impl IntoIterator<Item = AiModelId>,
        capabilities: impl IntoIterator<Item = AiCapability>,
    ) -> Result<Self, AiError> {
        Self::build(
            id,
            models,
            capabilities,
            AiProviderKind::FakeLocal,
            false,
            None,
        )
    }

    pub fn remote_unsupported(
        id: AiProviderId,
        models: impl IntoIterator<Item = AiModelId>,
        capabilities: impl IntoIterator<Item = AiCapability>,
        requires_network: bool,
        logical_secret_name: Option<&str>,
        secret_availability: SecretAvailability,
    ) -> Result<Self, AiError> {
        let secret = logical_secret_name
            .map(|name| SecretHandle::new(name, secret_availability))
            .transpose()?;
        Self::build(
            id,
            models,
            capabilities,
            AiProviderKind::RemoteUnsupported,
            requires_network,
            secret,
        )
    }

    fn build(
        id: AiProviderId,
        models: impl IntoIterator<Item = AiModelId>,
        capabilities: impl IntoIterator<Item = AiCapability>,
        kind: AiProviderKind,
        requires_network: bool,
        secret: Option<SecretHandle>,
    ) -> Result<Self, AiError> {
        let models = models.into_iter().collect::<BTreeSet<_>>();
        let capabilities = capabilities.into_iter().collect::<BTreeSet<_>>();
        if models.is_empty() || capabilities.is_empty() {
            return Err(AiError::EmptyProviderContract);
        }
        Ok(Self {
            id,
            models,
            capabilities,
            kind,
            requires_network,
            secret,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AiProviderRegistry {
    providers: BTreeMap<AiProviderId, AiProviderConfig>,
    network_allowed: bool,
    maximum_providers: usize,
}

impl AiProviderRegistry {
    #[must_use]
    pub fn new(network_allowed: bool, maximum_providers: usize) -> Self {
        Self {
            providers: BTreeMap::new(),
            network_allowed,
            maximum_providers,
        }
    }

    pub fn register(&mut self, provider: AiProviderConfig) -> Result<(), AiError> {
        if self.providers.contains_key(&provider.id) {
            return Err(AiError::DuplicateProvider);
        }
        if self.providers.len() >= self.maximum_providers {
            return Err(AiError::ProviderLimit);
        }
        self.providers.insert(provider.id.clone(), provider);
        Ok(())
    }

    fn authorize(
        &self,
        provider: &AiProviderId,
        model: &AiModelId,
        capability: AiCapability,
    ) -> Result<&AiProviderConfig, AiError> {
        let provider = self
            .providers
            .get(provider)
            .ok_or(AiError::UnknownProvider)?;
        if !provider.models.contains(model) {
            return Err(AiError::UnknownModel);
        }
        if !provider.capabilities.contains(&capability) {
            return Err(AiError::CapabilityDenied);
        }
        if provider.requires_network && !self.network_allowed {
            return Err(AiError::NetworkDenied);
        }
        if provider
            .secret
            .as_ref()
            .is_some_and(|secret| secret.availability == SecretAvailability::Missing)
        {
            return Err(AiError::SecretUnavailable);
        }
        if provider.kind == AiProviderKind::RemoteUnsupported {
            return Err(AiError::RemoteTransportUnsupported);
        }
        Ok(provider)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiTextRange {
    pub start: usize,
    pub end: usize,
}

impl AiTextRange {
    pub fn new(start: usize, end: usize) -> Result<Self, AiError> {
        if start > end {
            return Err(AiError::InvalidRange);
        }
        Ok(Self { start, end })
    }
}

/// Opaque host-private identity. It is never exposed to a provider transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiHostBufferId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiBufferContext {
    pub buffer: AiHostBufferId,
    pub revision: u64,
    pub cursor: usize,
    pub range: Option<AiTextRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiRequestContext {
    pub workspace_root_revision: u64,
    pub buffer: Option<AiBufferContext>,
    /// Bounded, immutable context selected by the host.
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiRequest {
    pub provider: AiProviderId,
    pub model: AiModelId,
    pub capability: AiCapability,
    pub context: AiRequestContext,
    pub prompt: String,
    /// Absolute host-monotonic deadline in milliseconds.
    pub deadline_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiTextEdit {
    pub range: AiTextRange,
    pub replacement: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiGhostSuggestion {
    pub buffer: AiHostBufferId,
    pub buffer_revision: u64,
    pub cursor: usize,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiComposerProposal {
    pub workspace_root_revision: u64,
    pub buffer: AiHostBufferId,
    pub buffer_revision: u64,
    pub edits: Vec<AiTextEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AiOutput {
    Chat(String),
    GhostText(AiGhostSuggestion),
    Composer(AiComposerProposal),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AiStreamState {
    Running,
    Complete(AiOutput),
    Failed(AiFailure),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiFailure {
    Provider,
    Deadline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiStreamSnapshot {
    pub request: AiRequestId,
    pub revision: AiStreamRevision,
    pub expected_sequence: u32,
    pub chunks: usize,
    pub text: String,
    pub state: AiStreamState,
    context: AiRequestContext,
}

impl AiStreamSnapshot {
    #[must_use]
    pub fn validate_ownership(
        &self,
        workspace_root_revision: u64,
        buffer: Option<(AiHostBufferId, u64)>,
    ) -> bool {
        if self.context.workspace_root_revision != workspace_root_revision {
            return false;
        }
        match (self.context.buffer, buffer) {
            (None, None) => true,
            (Some(expected), Some((buffer, revision))) => {
                expected.buffer == buffer && expected.revision == revision
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AiProviderEvent {
    Chunk {
        request: AiRequestId,
        sequence: u32,
        text: String,
    },
    Complete {
        request: AiRequestId,
    },
    Failed {
        request: AiRequestId,
    },
    Cancelled {
        request: AiRequestId,
    },
}

impl AiProviderEvent {
    fn request(&self) -> AiRequestId {
        match self {
            Self::Chunk { request, .. }
            | Self::Complete { request }
            | Self::Failed { request }
            | Self::Cancelled { request } => *request,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeChunk {
    pub delay_millis: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FakeTerminal {
    Complete,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeProviderScript {
    pub chunks: Vec<FakeChunk>,
    pub terminal: FakeTerminal,
    /// Typed edits used only for a composer completion.
    pub composer_edits: Vec<AiTextEdit>,
}

#[derive(Clone, Debug)]
struct QueuedEvent {
    ready_at: u64,
    event: AiProviderEvent,
}

#[derive(Clone, Debug)]
struct ActiveStream {
    request: AiRequest,
    snapshot: AiStreamSnapshot,
    composer_edits: Vec<AiTextEdit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiLimits {
    pub maximum_prompt_bytes: usize,
    pub maximum_context_bytes: usize,
    pub maximum_chunks: usize,
    pub maximum_output_bytes: usize,
    pub maximum_composer_edits: usize,
    pub maximum_edit_bytes: usize,
    pub maximum_queued_events: usize,
}

impl Default for AiLimits {
    fn default() -> Self {
        Self {
            maximum_prompt_bytes: 16 * 1024,
            maximum_context_bytes: 64 * 1024,
            maximum_chunks: 256,
            maximum_output_bytes: 256 * 1024,
            maximum_composer_edits: 128,
            maximum_edit_bytes: 256 * 1024,
            maximum_queued_events: 512,
        }
    }
}

/// Queued broker for one exact active generation.
///
/// A caller may replace the active request, but events from all prior request IDs
/// then fail closed. `apply_event` clone-validates before committing any state.
#[derive(Clone, Debug)]
pub struct AiProviderService {
    registry: AiProviderRegistry,
    limits: AiLimits,
    next_request: u64,
    next_revision: u64,
    active: Option<ActiveStream>,
    queue: VecDeque<QueuedEvent>,
    processed_events: u64,
}

impl AiProviderService {
    #[must_use]
    pub fn new(registry: AiProviderRegistry, limits: AiLimits) -> Self {
        Self {
            registry,
            limits,
            next_request: 1,
            next_revision: 1,
            active: None,
            queue: VecDeque::new(),
            processed_events: 0,
        }
    }

    pub fn start_fake(
        &mut self,
        request: AiRequest,
        script: FakeProviderScript,
        now_millis: u64,
    ) -> Result<AiRequestId, AiError> {
        self.registry
            .authorize(&request.provider, &request.model, request.capability)?;
        self.validate_request(&request, &script, now_millis)?;
        let event_count = script.chunks.len().saturating_add(1);
        if event_count > self.limits.maximum_queued_events {
            return Err(AiError::QueueLimit);
        }
        let id = AiRequestId(self.next_request);
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or(AiError::IdExhausted)?;
        self.queue.clear();
        let mut ready_at = now_millis;
        for (sequence, chunk) in script.chunks.iter().enumerate() {
            ready_at = ready_at.saturating_add(chunk.delay_millis);
            self.queue.push_back(QueuedEvent {
                ready_at,
                event: AiProviderEvent::Chunk {
                    request: id,
                    sequence: u32::try_from(sequence).map_err(|_| AiError::ChunkLimit)?,
                    text: chunk.text.clone(),
                },
            });
        }
        self.queue.push_back(QueuedEvent {
            ready_at,
            event: match script.terminal {
                FakeTerminal::Complete => AiProviderEvent::Complete { request: id },
                FakeTerminal::Failed => AiProviderEvent::Failed { request: id },
            },
        });
        let revision = self.take_revision()?;
        self.active = Some(ActiveStream {
            snapshot: AiStreamSnapshot {
                request: id,
                revision,
                expected_sequence: 0,
                chunks: 0,
                text: String::new(),
                state: AiStreamState::Running,
                context: request.context.clone(),
            },
            request,
            composer_edits: script.composer_edits,
        });
        Ok(id)
    }

    fn validate_request(
        &self,
        request: &AiRequest,
        script: &FakeProviderScript,
        now_millis: u64,
    ) -> Result<(), AiError> {
        if request.prompt.len() > self.limits.maximum_prompt_bytes {
            return Err(AiError::PromptLimit);
        }
        if request.context.text.len() > self.limits.maximum_context_bytes {
            return Err(AiError::ContextLimit);
        }
        if request.deadline_millis <= now_millis {
            return Err(AiError::Deadline);
        }
        if script.chunks.len() > self.limits.maximum_chunks {
            return Err(AiError::ChunkLimit);
        }
        let output = script
            .chunks
            .iter()
            .try_fold(0usize, |total, chunk| total.checked_add(chunk.text.len()))
            .ok_or(AiError::OutputLimit)?;
        if output > self.limits.maximum_output_bytes {
            return Err(AiError::OutputLimit);
        }
        if script.composer_edits.len() > self.limits.maximum_composer_edits {
            return Err(AiError::EditLimit);
        }
        let edit_bytes = script
            .composer_edits
            .iter()
            .try_fold(0usize, |total, edit| {
                total.checked_add(edit.replacement.len())
            })
            .ok_or(AiError::EditLimit)?;
        if edit_bytes > self.limits.maximum_edit_bytes {
            return Err(AiError::EditLimit);
        }
        match request.capability {
            AiCapability::Chat if !script.composer_edits.is_empty() => {
                Err(AiError::UnexpectedEdits)
            }
            AiCapability::GhostText if request.context.buffer.is_none() => {
                Err(AiError::BufferContextRequired)
            }
            AiCapability::GhostText if !script.composer_edits.is_empty() => {
                Err(AiError::UnexpectedEdits)
            }
            AiCapability::Composer if request.context.buffer.is_none() => {
                Err(AiError::BufferContextRequired)
            }
            AiCapability::Composer if script.composer_edits.is_empty() => {
                Err(AiError::ComposerEditsRequired)
            }
            _ => Ok(()),
        }
    }

    pub fn apply_event(&mut self, event: AiProviderEvent, now_millis: u64) -> Result<(), AiError> {
        let active = self.active.as_ref().ok_or(AiError::NoActiveRequest)?;
        if active.snapshot.request != event.request() {
            return Err(AiError::StaleRequest);
        }
        if !matches!(active.snapshot.state, AiStreamState::Running) {
            return Err(AiError::RequestFinished);
        }
        if now_millis > active.request.deadline_millis {
            let mut candidate = active.clone();
            candidate.snapshot.state = AiStreamState::Failed(AiFailure::Deadline);
            candidate.snapshot.revision = self.take_revision()?;
            self.active = Some(candidate);
            return Err(AiError::Deadline);
        }
        let mut candidate = active.clone();
        match event {
            AiProviderEvent::Chunk { sequence, text, .. } => {
                if sequence != candidate.snapshot.expected_sequence {
                    return Err(AiError::SequenceMismatch);
                }
                if candidate.snapshot.chunks >= self.limits.maximum_chunks {
                    return Err(AiError::ChunkLimit);
                }
                let length = candidate
                    .snapshot
                    .text
                    .len()
                    .checked_add(text.len())
                    .ok_or(AiError::OutputLimit)?;
                if length > self.limits.maximum_output_bytes {
                    return Err(AiError::OutputLimit);
                }
                candidate.snapshot.text.push_str(&text);
                candidate.snapshot.chunks += 1;
                candidate.snapshot.expected_sequence = candidate
                    .snapshot
                    .expected_sequence
                    .checked_add(1)
                    .ok_or(AiError::ChunkLimit)?;
            }
            AiProviderEvent::Complete { .. } => {
                let output = match candidate.request.capability {
                    AiCapability::Chat => AiOutput::Chat(candidate.snapshot.text.clone()),
                    AiCapability::GhostText => {
                        let context = candidate
                            .request
                            .context
                            .buffer
                            .ok_or(AiError::BufferContextRequired)?;
                        AiOutput::GhostText(AiGhostSuggestion {
                            buffer: context.buffer,
                            buffer_revision: context.revision,
                            cursor: context.cursor,
                            text: candidate.snapshot.text.clone(),
                        })
                    }
                    AiCapability::Composer => {
                        let context = candidate
                            .request
                            .context
                            .buffer
                            .ok_or(AiError::BufferContextRequired)?;
                        AiOutput::Composer(AiComposerProposal {
                            workspace_root_revision: candidate
                                .request
                                .context
                                .workspace_root_revision,
                            buffer: context.buffer,
                            buffer_revision: context.revision,
                            edits: candidate.composer_edits.clone(),
                        })
                    }
                };
                candidate.snapshot.state = AiStreamState::Complete(output);
            }
            AiProviderEvent::Failed { .. } => {
                candidate.snapshot.state = AiStreamState::Failed(AiFailure::Provider);
            }
            AiProviderEvent::Cancelled { .. } => {
                candidate.snapshot.state = AiStreamState::Cancelled;
            }
        }
        candidate.snapshot.revision = self.take_revision()?;
        self.active = Some(candidate);
        self.processed_events = self.processed_events.saturating_add(1);
        Ok(())
    }

    /// Applies at most one ready queued provider event.
    pub fn process_one(&mut self, now_millis: u64) -> Result<bool, AiError> {
        let Some(front) = self.queue.front() else {
            return Ok(false);
        };
        if front.ready_at > now_millis {
            if self
                .active
                .as_ref()
                .is_some_and(|active| now_millis > active.request.deadline_millis)
            {
                let request = self
                    .active
                    .as_ref()
                    .expect("checked active")
                    .snapshot
                    .request;
                self.apply_event(AiProviderEvent::Failed { request }, now_millis)?;
            }
            return Ok(false);
        }
        let queued = self.queue.pop_front().expect("front existed");
        self.apply_event(queued.event, now_millis)?;
        Ok(true)
    }

    pub fn cancel(&mut self, request: AiRequestId) -> Result<(), AiError> {
        let active = self.active.as_ref().ok_or(AiError::NoActiveRequest)?;
        if active.snapshot.request != request {
            return Err(AiError::StaleRequest);
        }
        if !matches!(active.snapshot.state, AiStreamState::Running) {
            return Err(AiError::RequestFinished);
        }
        let mut candidate = active.clone();
        candidate.snapshot.state = AiStreamState::Cancelled;
        candidate.snapshot.revision = self.take_revision()?;
        self.queue
            .retain(|queued| queued.event.request() != request);
        self.active = Some(candidate);
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<&AiStreamSnapshot> {
        self.active.as_ref().map(|active| &active.snapshot)
    }

    #[must_use]
    pub fn snapshot_if_changed(&self, accepted: AiStreamRevision) -> Option<&AiStreamSnapshot> {
        self.snapshot()
            .filter(|snapshot| snapshot.revision != accepted)
    }

    #[must_use]
    pub const fn processed_events(&self) -> u64 {
        self.processed_events
    }

    fn take_revision(&mut self) -> Result<AiStreamRevision, AiError> {
        let revision = AiStreamRevision(self.next_revision);
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(AiError::IdExhausted)?;
        Ok(revision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiError {
    InvalidIdentifier,
    InvalidSecretName,
    EmptyProviderContract,
    DuplicateProvider,
    ProviderLimit,
    UnknownProvider,
    UnknownModel,
    CapabilityDenied,
    NetworkDenied,
    SecretUnavailable,
    RemoteTransportUnsupported,
    InvalidRange,
    PromptLimit,
    ContextLimit,
    Deadline,
    ChunkLimit,
    OutputLimit,
    EditLimit,
    QueueLimit,
    BufferContextRequired,
    ComposerEditsRequired,
    UnexpectedEdits,
    NoActiveRequest,
    StaleRequest,
    RequestFinished,
    SequenceMismatch,
    IdExhausted,
}

impl fmt::Display for AiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentifier => "invalid AI identifier",
            Self::InvalidSecretName => "invalid logical secret name",
            Self::EmptyProviderContract => "AI provider contract must not be empty",
            Self::DuplicateProvider => "AI provider is already registered",
            Self::ProviderLimit => "AI provider limit exceeded",
            Self::UnknownProvider => "AI provider is unavailable",
            Self::UnknownModel => "AI model is unavailable",
            Self::CapabilityDenied => "AI capability is denied",
            Self::NetworkDenied => "AI network capability is denied",
            Self::SecretUnavailable => "required AI secret is unavailable",
            Self::RemoteTransportUnsupported => "remote AI transport is unsupported",
            Self::InvalidRange => "invalid AI text range",
            Self::PromptLimit => "AI prompt limit exceeded",
            Self::ContextLimit => "AI context limit exceeded",
            Self::Deadline => "AI request deadline exceeded",
            Self::ChunkLimit => "AI chunk limit exceeded",
            Self::OutputLimit => "AI output limit exceeded",
            Self::EditLimit => "AI edit proposal limit exceeded",
            Self::QueueLimit => "AI event queue limit exceeded",
            Self::BufferContextRequired => "AI buffer context is required",
            Self::ComposerEditsRequired => "AI composer edits are required",
            Self::UnexpectedEdits => "AI edits are not valid for this capability",
            Self::NoActiveRequest => "no AI request is active",
            Self::StaleRequest => "stale AI request",
            Self::RequestFinished => "AI request is already finished",
            Self::SequenceMismatch => "AI stream sequence mismatch",
            Self::IdExhausted => "AI identity space exhausted",
        })
    }
}

impl std::error::Error for AiError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> (AiProviderId, AiModelId, AiProviderConfig) {
        let id = AiProviderId::new("fake.local").unwrap();
        let model = AiModelId::new("deterministic-v1").unwrap();
        let config = AiProviderConfig::fake_local(
            id.clone(),
            [model.clone()],
            [
                AiCapability::Chat,
                AiCapability::GhostText,
                AiCapability::Composer,
            ],
        )
        .unwrap();
        (id, model, config)
    }

    fn service(limits: AiLimits) -> (AiProviderService, AiProviderId, AiModelId) {
        let (id, model, provider) = provider();
        let mut registry = AiProviderRegistry::new(false, 8);
        registry.register(provider).unwrap();
        (AiProviderService::new(registry, limits), id, model)
    }

    fn request(
        provider: AiProviderId,
        model: AiModelId,
        capability: AiCapability,
        deadline_millis: u64,
    ) -> AiRequest {
        AiRequest {
            provider,
            model,
            capability,
            context: AiRequestContext {
                workspace_root_revision: 7,
                buffer: Some(AiBufferContext {
                    buffer: AiHostBufferId(11),
                    revision: 13,
                    cursor: 5,
                    range: None,
                }),
                text: "context".into(),
            },
            prompt: "prompt".into(),
            deadline_millis,
        }
    }

    fn script(chunks: &[&str]) -> FakeProviderScript {
        FakeProviderScript {
            chunks: chunks
                .iter()
                .map(|text| FakeChunk {
                    delay_millis: 0,
                    text: (*text).to_owned(),
                })
                .collect(),
            terminal: FakeTerminal::Complete,
            composer_edits: Vec::new(),
        }
    }

    fn drain(service: &mut AiProviderService, now: u64) {
        while service.process_one(now).unwrap() {}
    }

    #[test]
    fn unicode_fragments_commit_in_exact_order() {
        let (mut service, provider, model) = service(AiLimits::default());
        let id = service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                script(&["界", "e\u{301}", "🙂"]),
                0,
            )
            .unwrap();
        drain(&mut service, 1);
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.request, id);
        assert_eq!(snapshot.text, "界e\u{301}🙂");
        assert_eq!(snapshot.expected_sequence, 3);
        assert_eq!(
            snapshot.state,
            AiStreamState::Complete(AiOutput::Chat(snapshot.text.clone()))
        );
    }

    #[test]
    fn duplicate_and_out_of_order_chunks_reject_without_partial_commit() {
        let (mut service, provider, model) = service(AiLimits::default());
        let id = service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                script(&[]),
                0,
            )
            .unwrap();
        service
            .apply_event(
                AiProviderEvent::Chunk {
                    request: id,
                    sequence: 0,
                    text: "accepted".into(),
                },
                1,
            )
            .unwrap();
        let before = service.snapshot().unwrap().clone();
        for sequence in [0, 2] {
            assert_eq!(
                service.apply_event(
                    AiProviderEvent::Chunk {
                        request: id,
                        sequence,
                        text: "rejected".into(),
                    },
                    1,
                ),
                Err(AiError::SequenceMismatch)
            );
            assert_eq!(service.snapshot(), Some(&before));
        }
    }

    #[test]
    fn request_and_stream_ceilings_fail_closed() {
        let limits = AiLimits {
            maximum_prompt_bytes: 6,
            maximum_context_bytes: 7,
            maximum_chunks: 1,
            maximum_output_bytes: 3,
            maximum_composer_edits: 1,
            maximum_edit_bytes: 3,
            maximum_queued_events: 2,
        };
        let (mut service, provider, model) = service(limits);
        let mut value = request(provider.clone(), model.clone(), AiCapability::Chat, 100);
        value.prompt = "seven!!".into();
        assert_eq!(
            service.start_fake(value, script(&[]), 0),
            Err(AiError::PromptLimit)
        );
        let mut value = request(provider.clone(), model.clone(), AiCapability::Chat, 100);
        value.context.text = "eight!!!".into();
        assert_eq!(
            service.start_fake(value, script(&[]), 0),
            Err(AiError::ContextLimit)
        );
        let value = request(provider.clone(), model.clone(), AiCapability::Chat, 100);
        assert_eq!(
            service.start_fake(value, script(&["a", "b"]), 0),
            Err(AiError::ChunkLimit)
        );
        let value = request(provider.clone(), model.clone(), AiCapability::Chat, 100);
        assert_eq!(
            service.start_fake(value, script(&["four"]), 0),
            Err(AiError::OutputLimit)
        );
        let value = request(provider, model, AiCapability::Chat, 0);
        assert_eq!(
            service.start_fake(value, script(&[]), 0),
            Err(AiError::Deadline)
        );
        assert!(service.snapshot().is_none());
    }

    #[test]
    fn cancellation_removes_queue_and_suppresses_late_events() {
        let (mut service, provider, model) = service(AiLimits::default());
        let id = service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                script(&["late"]),
                0,
            )
            .unwrap();
        service.cancel(id).unwrap();
        let revision = service.snapshot().unwrap().revision;
        assert!(!service.process_one(1).unwrap());
        assert_eq!(
            service.apply_event(
                AiProviderEvent::Chunk {
                    request: id,
                    sequence: 0,
                    text: "late".into(),
                },
                1,
            ),
            Err(AiError::RequestFinished)
        );
        assert_eq!(service.snapshot().unwrap().revision, revision);
        assert!(matches!(
            service.snapshot().unwrap().state,
            AiStreamState::Cancelled
        ));
    }

    #[test]
    fn requery_replaces_generation_and_rejects_prior_events() {
        let (mut service, provider, model) = service(AiLimits::default());
        let old = service
            .start_fake(
                request(provider.clone(), model.clone(), AiCapability::Chat, 100),
                script(&["old"]),
                0,
            )
            .unwrap();
        let new = service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                script(&["new"]),
                0,
            )
            .unwrap();
        assert!(new.0 > old.0);
        let before = service.snapshot().unwrap().clone();
        assert_eq!(
            service.apply_event(
                AiProviderEvent::Chunk {
                    request: old,
                    sequence: 0,
                    text: "stale".into()
                },
                1
            ),
            Err(AiError::StaleRequest)
        );
        assert_eq!(service.snapshot(), Some(&before));
        drain(&mut service, 1);
        assert_eq!(service.snapshot().unwrap().text, "new");
    }

    #[test]
    fn delayed_event_expires_at_bounded_deadline() {
        let (mut service, provider, model) = service(AiLimits::default());
        let mut delayed = script(&["late"]);
        delayed.chunks[0].delay_millis = 20;
        service
            .start_fake(request(provider, model, AiCapability::Chat, 10), delayed, 0)
            .unwrap();
        assert_eq!(service.process_one(11), Err(AiError::Deadline));
        assert!(matches!(
            service.snapshot().unwrap().state,
            AiStreamState::Failed(AiFailure::Deadline)
        ));
    }

    #[test]
    fn scripted_provider_failure_is_typed_and_bounded() {
        let (mut service, provider, model) = service(AiLimits::default());
        let mut failure = script(&["partial"]);
        failure.terminal = FakeTerminal::Failed;
        service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                failure,
                0,
            )
            .unwrap();
        drain(&mut service, 1);
        assert_eq!(service.snapshot().unwrap().text, "partial");
        assert!(matches!(
            service.snapshot().unwrap().state,
            AiStreamState::Failed(AiFailure::Provider)
        ));
    }

    #[test]
    fn registry_denies_capability_network_secret_and_remote_transport() {
        let model = AiModelId::new("model").unwrap();
        let chat = [AiCapability::Chat];
        let mut registry = AiProviderRegistry::new(false, 8);
        let (fake_id, _, fake) = provider();
        registry.register(fake).unwrap();
        assert_eq!(
            registry.authorize(
                &fake_id,
                &AiModelId::new("deterministic-v1").unwrap(),
                AiCapability::Composer
            ),
            Ok(registry.providers.get(&fake_id).unwrap())
        );
        let network_id = AiProviderId::new("remote.network").unwrap();
        registry
            .register(
                AiProviderConfig::remote_unsupported(
                    network_id.clone(),
                    [model.clone()],
                    chat,
                    true,
                    None,
                    SecretAvailability::Missing,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            registry.authorize(&network_id, &model, AiCapability::Chat),
            Err(AiError::NetworkDenied)
        );
        let secret_id = AiProviderId::new("remote.secret").unwrap();
        registry
            .register(
                AiProviderConfig::remote_unsupported(
                    secret_id.clone(),
                    [model.clone()],
                    chat,
                    false,
                    Some("PRIVATE_TOKEN"),
                    SecretAvailability::Missing,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            registry.authorize(&secret_id, &model, AiCapability::Chat),
            Err(AiError::SecretUnavailable)
        );
        let remote_id = AiProviderId::new("remote.ready").unwrap();
        registry
            .register(
                AiProviderConfig::remote_unsupported(
                    remote_id.clone(),
                    [model.clone()],
                    chat,
                    false,
                    Some("PRIVATE_TOKEN"),
                    SecretAvailability::Available,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            registry.authorize(&remote_id, &model, AiCapability::Chat),
            Err(AiError::RemoteTransportUnsupported)
        );
        let limited_id = AiProviderId::new("fake.chat").unwrap();
        registry
            .register(
                AiProviderConfig::fake_local(limited_id.clone(), [model.clone()], chat).unwrap(),
            )
            .unwrap();
        assert_eq!(
            registry.authorize(&limited_id, &model, AiCapability::Composer),
            Err(AiError::CapabilityDenied)
        );
    }

    #[test]
    fn logical_secret_validation_and_diagnostics_are_redacted() {
        assert_eq!(
            AiProviderConfig::remote_unsupported(
                AiProviderId::new("remote").unwrap(),
                [AiModelId::new("model").unwrap()],
                [AiCapability::Chat],
                true,
                Some("not-valid"),
                SecretAvailability::Available,
            ),
            Err(AiError::InvalidSecretName)
        );
        let config = AiProviderConfig::remote_unsupported(
            AiProviderId::new("remote").unwrap(),
            [AiModelId::new("model").unwrap()],
            [AiCapability::Chat],
            true,
            Some("ULTRA_PRIVATE_KEY"),
            SecretAvailability::Available,
        )
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("ULTRA_PRIVATE_KEY"));
        assert!(debug.contains("[REDACTED]"));
        for error in [AiError::NetworkDenied, AiError::SecretUnavailable] {
            assert!(!error.to_string().contains("PRIVATE"));
        }
    }

    #[test]
    fn ghost_and_composer_outputs_remain_exact_revision_owned() {
        let (mut service, provider, model) = service(AiLimits::default());
        service
            .start_fake(
                request(
                    provider.clone(),
                    model.clone(),
                    AiCapability::GhostText,
                    100,
                ),
                script(&["suggestion"]),
                0,
            )
            .unwrap();
        drain(&mut service, 1);
        let ghost = service.snapshot().unwrap();
        assert!(ghost.validate_ownership(7, Some((AiHostBufferId(11), 13))));
        assert!(!ghost.validate_ownership(8, Some((AiHostBufferId(11), 13))));
        assert!(!ghost.validate_ownership(7, Some((AiHostBufferId(11), 14))));

        let mut composer = script(&["proposal"]);
        composer.composer_edits = vec![AiTextEdit {
            range: AiTextRange::new(0, 4).unwrap(),
            replacement: "edit".into(),
        }];
        service
            .start_fake(
                request(provider, model, AiCapability::Composer, 100),
                composer,
                0,
            )
            .unwrap();
        drain(&mut service, 1);
        let snapshot = service.snapshot().unwrap();
        assert!(snapshot.validate_ownership(7, Some((AiHostBufferId(11), 13))));
        assert!(!snapshot.validate_ownership(7, Some((AiHostBufferId(12), 13))));
        let AiStreamState::Complete(AiOutput::Composer(proposal)) = &snapshot.state else {
            panic!("composer proposal")
        };
        assert_eq!(proposal.workspace_root_revision, 7);
        assert_eq!(proposal.buffer_revision, 13);
        assert_eq!(proposal.edits.len(), 1);
    }

    #[test]
    fn unchanged_reads_do_no_work_and_publish_no_revision() {
        let (mut service, provider, model) = service(AiLimits::default());
        service
            .start_fake(
                request(provider, model, AiCapability::Chat, 100),
                script(&["done"]),
                0,
            )
            .unwrap();
        drain(&mut service, 1);
        let revision = service.snapshot().unwrap().revision;
        let processed = service.processed_events();
        for _ in 0..256 {
            assert!(service.snapshot_if_changed(revision).is_none());
            assert_eq!(service.snapshot().unwrap().revision, revision);
        }
        assert_eq!(service.processed_events(), processed);
    }
}
