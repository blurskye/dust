use super::*;

pub mod component_api {
    wasmtime::component::bindgen!({ path: "wit", world: "editor-plugin" });
}

pub struct ComponentHostState {
    pub limits: wasmtime::StoreLimits,
    pub capabilities: BTreeSet<PluginCapability>,
    pub declared_content_types: BTreeSet<String>,
    pub snapshot: Option<component_api::dust::plugin::host::Snapshot>,
    pub proposals: Vec<(u64, u64, Vec<component_api::dust::plugin::host::Edit>)>,
    pub messages: Vec<String>,
    pub config: BTreeMap<String, String>,
    pub ui_revision: Option<u64>,
    pub confirmation_available: bool,
    pub confirmation_result_only: bool,
    pub ui_requests: Vec<PluginUiRequest>,
    pub ui_events: Vec<component_api::dust::plugin::host::UiEvent>,
    pub next_ui_token: u64,
    pub confirmation_requests: Vec<PluginConfirmationRequest>,
    pub confirmation_results: Vec<component_api::dust::plugin::host::ConfirmationResult>,
    pub next_confirmation_token: u64,
    pub host_events: Vec<component_api::dust::plugin::host::HostEvent>,
    pub delivered_host_event_ids: BTreeSet<u64>,
    pub host_event_acknowledgements: Vec<u64>,
    pub storage_policy: Option<PluginStorageContribution>,
    pub storage_requests: Vec<PluginStorageRequest>,
    pub storage_results: Vec<component_api::dust::plugin::host::StorageResult>,
    pub next_storage_token: u64,
    pub provider_requests: Vec<component_api::dust::plugin::host::PickerProviderRequest>,
    pub delivered_provider_requests: BTreeSet<u64>,
    pub provider_chunks: Vec<PluginPickerChunk>,
    pub virtual_document_requests: Vec<component_api::dust::plugin::host::VirtualDocumentRequest>,
    pub delivered_virtual_document_requests: BTreeSet<u64>,
    pub virtual_document_proposals: Vec<PluginVirtualDocumentProposal>,
}
impl component_api::dust::plugin::host::Host for ComponentHostState {
    fn current_buffer(&mut self) -> Result<component_api::dust::plugin::host::Snapshot, String> {
        if self
            .capabilities
            .contains(&PluginCapability::ReadCurrentBuffer)
        {
            self.snapshot
                .clone()
                .ok_or_else(|| "no current buffer snapshot".into())
        } else {
            Err("capability denied".into())
        }
    }
    fn propose_edits(
        &mut self,
        buffer: u64,
        revision: u64,
        edits: Vec<component_api::dust::plugin::host::Edit>,
    ) -> Result<u64, String> {
        if self.confirmation_result_only {
            return Err("confirmation result cannot propose edits".into());
        }
        if !self.capabilities.contains(&PluginCapability::ProposeEdits) {
            return Err("capability denied".into());
        }
        if edits.len() > 4096 {
            return Err("edit ceiling exceeded".into());
        }
        if edits
            .iter()
            .any(|edit| edit.start > edit.end || edit.replacement.len() > 1024 * 1024)
        {
            return Err("invalid edit proposal".into());
        }
        // Bound the complete guest-produced batch, not just each host call.
        if self.proposals.len() >= 1024
            || self
                .proposals
                .iter()
                .map(|(_, _, edits)| edits.len())
                .sum::<usize>()
                + edits.len()
                > 4096
            || self
                .proposals
                .iter()
                .flat_map(|(_, _, edits)| edits)
                .chain(&edits)
                .try_fold(0usize, |bytes, edit| {
                    bytes.checked_add(edit.replacement.len())
                })
                .is_none_or(|bytes| bytes > 1024 * 1024)
        {
            return Err("edit proposal batch ceiling exceeded".into());
        }
        self.proposals.push((buffer, revision, edits));
        Ok(self.proposals.len() as u64)
    }
    fn show_message(&mut self, message: String) -> Result<(), String> {
        if !self.capabilities.contains(&PluginCapability::ShowMessage) {
            return Err("capability denied".into());
        }
        self.messages.push(
            message
                .chars()
                .filter(|character| !character.is_control())
                .take(4096)
                .collect(),
        );
        if self.messages.len() > 64 {
            self.messages.remove(0);
        }
        Ok(())
    }
    fn config_get(&mut self, key: String) -> Option<String> {
        self.capabilities
            .contains(&PluginCapability::ReadConfig)
            .then(|| self.config.get(&key).cloned())
            .flatten()
    }

    fn current_ui_revision(&mut self) -> Option<u64> {
        self.ui_revision
    }

    fn queue_ui_request(
        &mut self,
        request: component_api::dust::plugin::host::UiRequest,
    ) -> Result<u64, component_api::dust::plugin::host::UiError> {
        if self.confirmation_result_only
            && !matches!(
                request,
                component_api::dust::plugin::host::UiRequest::ReplaceUi(_)
                    | component_api::dust::plugin::host::UiRequest::ReplacePrivate(_)
                    | component_api::dust::plugin::host::UiRequest::PatchPrivate(_)
            )
        {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "confirmation result may update existing plugin UI only",
            )));
        }
        if self.ui_requests.len() >= 64 {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::QueueFull,
                "UI request ceiling exceeded",
            )));
        }
        let token = self.next_ui_token;
        self.next_ui_token = self.next_ui_token.checked_add(1).ok_or_else(|| {
            component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "UI request token exhausted",
            ))
        })?;
        let kind = convert_component_ui_request(request, &self.declared_content_types)
            .map_err(component_ui_error)?;
        self.ui_requests.push(PluginUiRequest { token, kind });
        Ok(token)
    }

    fn take_ui_events(&mut self) -> Vec<component_api::dust::plugin::host::UiEvent> {
        std::mem::take(&mut self.ui_events)
    }

    fn queue_confirmation(
        &mut self,
        request: component_api::dust::plugin::host::ConfirmationRequest,
    ) -> Result<u64, component_api::dust::plugin::host::ConfirmationError> {
        if !self.confirmation_available || !self.confirmation_requests.is_empty() {
            return Err(component_confirmation_error(PluginConfirmationError::new(
                PluginConfirmationErrorCode::QueueFull,
                "one confirmation may be requested per execution",
            )));
        }
        if request.title.is_empty()
            || request.title.len() > 128
            || request.message.is_empty()
            || request.message.len() > 4_096
            || request
                .title
                .chars()
                .chain(request.message.chars())
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
            || matches!(
                request.default_answer,
                component_api::dust::plugin::host::ConfirmationAnswer::Cancelled
            )
        {
            return Err(component_confirmation_error(PluginConfirmationError::new(
                PluginConfirmationErrorCode::InvalidRequest,
                "confirmation title, message, or default is invalid",
            )));
        }
        let token = self.next_confirmation_token;
        self.next_confirmation_token =
            self.next_confirmation_token.checked_add(1).ok_or_else(|| {
                component_confirmation_error(PluginConfirmationError::new(
                    PluginConfirmationErrorCode::Unavailable,
                    "confirmation token exhausted",
                ))
            })?;
        self.confirmation_requests.push(PluginConfirmationRequest {
            token,
            title: request.title,
            message: request.message,
            default_answer: internal_confirmation_answer(request.default_answer),
        });
        Ok(token)
    }

    fn take_confirmation_results(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::ConfirmationResult> {
        std::mem::take(&mut self.confirmation_results)
    }

    fn take_host_events(&mut self) -> Vec<component_api::dust::plugin::host::HostEvent> {
        if self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            std::mem::take(&mut self.host_events)
        } else {
            Vec::new()
        }
    }

    fn acknowledge_host_events(&mut self, ids: Vec<u64>) -> Result<(), String> {
        if !self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            return Err("capability denied".into());
        }
        if ids.len() > 64 {
            return Err("host event acknowledgement ceiling exceeded".into());
        }
        let mut unique = BTreeSet::new();
        for id in ids {
            if id == 0 || !self.delivered_host_event_ids.contains(&id) || !unique.insert(id) {
                return Err("invalid host event acknowledgement".into());
            }
        }
        self.host_event_acknowledgements.extend(unique);
        Ok(())
    }

    fn queue_storage_request(
        &mut self,
        request: component_api::dust::plugin::host::StorageRequest,
    ) -> Result<u64, component_api::dust::plugin::host::StorageError> {
        if self.confirmation_result_only {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "confirmation result cannot use storage",
            )));
        }
        if !self.capabilities.contains(&PluginCapability::PluginStorage) {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.storage_requests.len() >= STORAGE_REQUEST_CEILING {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::QueueFull,
                "storage request ceiling exceeded",
            )));
        }
        let token = self.next_storage_token;
        self.next_storage_token = self.next_storage_token.checked_add(1).ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage token exhausted",
            ))
        })?;
        let kind = convert_component_storage_request(request);
        let candidate = PluginStorageRequest { token, kind };
        let policy = self.storage_policy.as_ref().ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage policy is unavailable",
            ))
        })?;
        storage::validate_request(&candidate, policy).map_err(component_storage_error)?;
        self.storage_requests.push(candidate);
        Ok(token)
    }

    fn take_storage_results(&mut self) -> Vec<component_api::dust::plugin::host::StorageResult> {
        if self.capabilities.contains(&PluginCapability::PluginStorage) {
            std::mem::take(&mut self.storage_results)
        } else {
            Vec::new()
        }
    }

    fn take_picker_provider_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::PickerProviderRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            std::mem::take(&mut self.provider_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_picker_provider_chunk(
        &mut self,
        chunk: component_api::dust::plugin::host::PickerProviderChunk,
    ) -> Result<(), component_api::dust::plugin::host::ProviderError> {
        if self.confirmation_result_only {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Unavailable,
                "confirmation result cannot publish provider chunks",
            )));
        }
        if !self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.provider_chunks.len() >= PROVIDER_REQUEST_QUEUE_CEILING {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::QueueFull,
                "provider chunk queue is full",
            )));
        }
        if !self.delivered_provider_requests.contains(&chunk.request) {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::UnknownRequest,
                "provider request was not delivered in this execution",
            )));
        }
        let terminal_is_error = matches!(
            chunk.terminal.kind,
            component_api::dust::plugin::host::ProviderTerminalKind::Error
        );
        if terminal_is_error != chunk.terminal.error.is_some() {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Malformed,
                "provider terminal/error payload mismatch",
            )));
        }
        let chunk = convert_component_provider_chunk(chunk);
        providers::validate_chunk(&chunk).map_err(component_provider_error)?;
        self.provider_chunks.push(chunk);
        Ok(())
    }

    fn take_virtual_document_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::VirtualDocumentRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            std::mem::take(&mut self.virtual_document_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_virtual_document_proposal(
        &mut self,
        proposal: component_api::dust::plugin::host::VirtualDocumentProposal,
    ) -> Result<(), component_api::dust::plugin::host::VirtualDocumentError> {
        if self.confirmation_result_only {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ReadOnly,
                    "confirmation result cannot publish virtual documents",
                ),
            ));
        }
        if !self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ReadOnly,
                    "capability denied",
                ),
            ));
        }
        if self.virtual_document_proposals.len() >= VIRTUAL_DOCUMENT_REQUEST_QUEUE_CEILING {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::QueueFull,
                    "proposal queue full",
                ),
            ));
        }
        if !self
            .delivered_virtual_document_requests
            .contains(&proposal.request)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::UnknownRequest,
                    "request was not delivered in this execution",
                ),
            ));
        }
        let proposal = convert_component_virtual_document_proposal(proposal)
            .map_err(component_virtual_document_error)?;
        self.virtual_document_proposals.push(proposal);
        Ok(())
    }
}

pub(super) fn internal_confirmation_answer(
    answer: component_api::dust::plugin::host::ConfirmationAnswer,
) -> PluginConfirmationAnswer {
    use component_api::dust::plugin::host::ConfirmationAnswer as A;
    match answer {
        A::Yes => PluginConfirmationAnswer::Yes,
        A::No => PluginConfirmationAnswer::No,
        A::Cancelled => PluginConfirmationAnswer::Cancelled,
    }
}

pub(super) fn component_confirmation_answer(
    answer: PluginConfirmationAnswer,
) -> component_api::dust::plugin::host::ConfirmationAnswer {
    use component_api::dust::plugin::host::ConfirmationAnswer as A;
    match answer {
        PluginConfirmationAnswer::Yes => A::Yes,
        PluginConfirmationAnswer::No => A::No,
        PluginConfirmationAnswer::Cancelled => A::Cancelled,
    }
}

pub(super) fn component_confirmation_error(
    error: PluginConfirmationError,
) -> component_api::dust::plugin::host::ConfirmationError {
    use component_api::dust::plugin::host::ConfirmationErrorCode as C;
    component_api::dust::plugin::host::ConfirmationError {
        code: match error.code {
            PluginConfirmationErrorCode::QueueFull => C::QueueFull,
            PluginConfirmationErrorCode::InvalidRequest => C::InvalidRequest,
            PluginConfirmationErrorCode::GenerationLost => C::GenerationLost,
            PluginConfirmationErrorCode::SourceRetired => C::SourceRetired,
            PluginConfirmationErrorCode::Unavailable => C::Unavailable,
        },
        detail: error.detail,
    }
}

pub(super) fn component_confirmation_result(
    result: PluginConfirmationResult,
) -> component_api::dust::plugin::host::ConfirmationResult {
    use component_api::dust::plugin::host::ConfirmationOutcome as O;
    component_api::dust::plugin::host::ConfirmationResult {
        request: result.request,
        outcome: match result.outcome {
            PluginConfirmationOutcome::Answered(answer) => {
                O::Answered(component_confirmation_answer(answer))
            }
            PluginConfirmationOutcome::Failed(error) => {
                O::Failed(component_confirmation_error(error))
            }
        },
    }
}

pub(super) fn component_virtual_document_request(
    request: PluginVirtualDocumentRequest,
) -> component_api::dust::plugin::host::VirtualDocumentRequest {
    use component_api::dust::plugin::host::VirtualDocumentRequestKind as K;
    component_api::dust::plugin::host::VirtualDocumentRequest {
        request: request.request,
        document: request.document,
        source: request.source,
        key: request.key,
        kind: match request.kind {
            PluginVirtualDocumentRequestKind::Open => K::Open,
            PluginVirtualDocumentRequestKind::Refresh => K::Refresh,
            PluginVirtualDocumentRequestKind::CloseNotice => K::CloseNotice,
        },
        current_revision: request.current_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_generation: request.cancellation_generation,
        maximum_document_bytes: request.maximum_document_bytes,
        maximum_document_lines: request.maximum_document_lines,
        maximum_patch_bytes: request.maximum_patch_bytes,
        maximum_patches: request.maximum_patches,
    }
}
pub(super) fn convert_component_virtual_document_proposal(
    p: component_api::dust::plugin::host::VirtualDocumentProposal,
) -> Result<PluginVirtualDocumentProposal, PluginVirtualDocumentError> {
    use component_api::dust::plugin::host::VirtualDocumentProposalKind as K;
    let kind = match p.kind {
        K::Full(f) => {
            if f.title.len() > VIRTUAL_DOCUMENT_TITLE_BYTE_CEILING
                || f.language.len() > VIRTUAL_DOCUMENT_LANGUAGE_BYTE_CEILING
                || f.text.len() > MAXIMUM_VIRTUAL_DOCUMENT_BYTES as usize
            {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ByteCeiling,
                    "virtual document full proposal exceeds absolute ceiling",
                ));
            }
            PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                expected_revision: f.expected_revision,
                revision: f.revision,
                title: f.title,
                language: f.language,
                text: f.text,
            })
        }
        K::Patches(s) => {
            if s.patches.len() > 256 {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::PatchCeiling,
                    "virtual document patch count exceeds absolute ceiling",
                ));
            }
            let mut bytes = 0usize;
            let mut patches = Vec::with_capacity(s.patches.len());
            for p in s.patches {
                bytes = bytes.checked_add(p.replacement.len()).ok_or_else(|| {
                    PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchCeiling,
                        "patch byte overflow",
                    )
                })?;
                if bytes > MAXIMUM_VIRTUAL_PATCH_BYTES as usize {
                    return Err(PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchCeiling,
                        "patch bytes exceed absolute ceiling",
                    ));
                }
                patches.push(PluginVirtualDocumentPatch {
                    start: p.start,
                    end: p.end,
                    replacement: p.replacement,
                });
            }
            PluginVirtualDocumentProposalKind::Patches(PluginVirtualDocumentPatchSet {
                expected_revision: s.expected_revision,
                revision: s.revision,
                patches,
            })
        }
        K::CloseAcknowledged => PluginVirtualDocumentProposalKind::CloseAcknowledged,
        K::Error(e) => PluginVirtualDocumentProposalKind::Error(internal_virtual_document_error(e)),
    };
    Ok(PluginVirtualDocumentProposal {
        request: p.request,
        document: p.document,
        cancellation_generation: p.cancellation_generation,
        kind,
    })
}
pub(super) fn internal_virtual_document_error(
    e: component_api::dust::plugin::host::VirtualDocumentError,
) -> PluginVirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    PluginVirtualDocumentError::new(
        match e.code {
            C::QueueFull => PluginVirtualDocumentErrorCode::QueueFull,
            C::UnknownSource => PluginVirtualDocumentErrorCode::UnknownSource,
            C::UnknownRequest => PluginVirtualDocumentErrorCode::UnknownRequest,
            C::UnknownDocument => PluginVirtualDocumentErrorCode::UnknownDocument,
            C::StaleGeneration => PluginVirtualDocumentErrorCode::StaleGeneration,
            C::StaleRevision => PluginVirtualDocumentErrorCode::StaleRevision,
            C::Cancelled => PluginVirtualDocumentErrorCode::Cancelled,
            C::Expired => PluginVirtualDocumentErrorCode::Expired,
            C::InvalidTitle => PluginVirtualDocumentErrorCode::InvalidTitle,
            C::InvalidLanguage => PluginVirtualDocumentErrorCode::InvalidLanguage,
            C::InvalidUtf8Range => PluginVirtualDocumentErrorCode::InvalidUtf8Range,
            C::InvalidControl => PluginVirtualDocumentErrorCode::InvalidControl,
            C::ByteCeiling => PluginVirtualDocumentErrorCode::ByteCeiling,
            C::LineCeiling => PluginVirtualDocumentErrorCode::LineCeiling,
            C::PatchCeiling => PluginVirtualDocumentErrorCode::PatchCeiling,
            C::ReadOnly => PluginVirtualDocumentErrorCode::ReadOnly,
            C::Malformed => PluginVirtualDocumentErrorCode::Malformed,
            C::Internal => PluginVirtualDocumentErrorCode::Internal,
        },
        e.detail,
    )
}
pub(super) fn component_virtual_document_error(
    e: PluginVirtualDocumentError,
) -> component_api::dust::plugin::host::VirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    component_api::dust::plugin::host::VirtualDocumentError {
        code: match e.code {
            PluginVirtualDocumentErrorCode::QueueFull => C::QueueFull,
            PluginVirtualDocumentErrorCode::UnknownSource => C::UnknownSource,
            PluginVirtualDocumentErrorCode::UnknownRequest => C::UnknownRequest,
            PluginVirtualDocumentErrorCode::UnknownDocument => C::UnknownDocument,
            PluginVirtualDocumentErrorCode::StaleGeneration => C::StaleGeneration,
            PluginVirtualDocumentErrorCode::StaleRevision => C::StaleRevision,
            PluginVirtualDocumentErrorCode::Cancelled => C::Cancelled,
            PluginVirtualDocumentErrorCode::Expired => C::Expired,
            PluginVirtualDocumentErrorCode::InvalidTitle => C::InvalidTitle,
            PluginVirtualDocumentErrorCode::InvalidLanguage => C::InvalidLanguage,
            PluginVirtualDocumentErrorCode::InvalidUtf8Range => C::InvalidUtf8Range,
            PluginVirtualDocumentErrorCode::InvalidControl => C::InvalidControl,
            PluginVirtualDocumentErrorCode::ByteCeiling => C::ByteCeiling,
            PluginVirtualDocumentErrorCode::LineCeiling => C::LineCeiling,
            PluginVirtualDocumentErrorCode::PatchCeiling => C::PatchCeiling,
            PluginVirtualDocumentErrorCode::ReadOnly => C::ReadOnly,
            PluginVirtualDocumentErrorCode::Malformed => C::Malformed,
            PluginVirtualDocumentErrorCode::Internal => C::Internal,
        },
        detail: e.detail,
    }
}

pub(super) fn component_provider_request(
    request: PluginPickerRequestEnvelope,
) -> component_api::dust::plugin::host::PickerProviderRequest {
    component_api::dust::plugin::host::PickerProviderRequest {
        id: request.id,
        provider: request.provider,
        query: request.query,
        context: request.context,
        workspace_revision: request.workspace_revision,
        buffer_revision: request.buffer_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_generation: request.cancellation_generation,
        maximum_items: request.maximum_items,
        maximum_bytes: request.maximum_bytes,
    }
}

pub(super) fn convert_component_provider_chunk(
    chunk: component_api::dust::plugin::host::PickerProviderChunk,
) -> PluginPickerChunk {
    use component_api::dust::plugin::host::ProviderTerminalKind;
    let terminal = match chunk.terminal.kind {
        ProviderTerminalKind::More => PluginProviderTerminal::More,
        ProviderTerminalKind::Complete => PluginProviderTerminal::Complete,
        ProviderTerminalKind::Truncated => PluginProviderTerminal::Truncated,
        ProviderTerminalKind::Error => PluginProviderTerminal::Error(
            chunk
                .terminal
                .error
                .map(internal_provider_error)
                .unwrap_or_else(|| {
                    PluginProviderError::new(
                        PluginProviderErrorCode::Malformed,
                        "provider error terminal omitted its error",
                    )
                }),
        ),
    };
    PluginPickerChunk {
        request: chunk.request,
        sequence: chunk.sequence,
        items: chunk
            .items
            .into_iter()
            .map(|item| PluginPickerItem {
                id: item.id,
                primary: item.primary,
                secondary: item.secondary,
                action_data: item.action_data,
            })
            .collect(),
        terminal,
    }
}

pub(super) fn internal_provider_error(
    error: component_api::dust::plugin::host::ProviderError,
) -> PluginProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    PluginProviderError::new(
        match error.code {
            ProviderErrorCode::QueueFull => PluginProviderErrorCode::QueueFull,
            ProviderErrorCode::UnknownRequest => PluginProviderErrorCode::UnknownRequest,
            ProviderErrorCode::UnknownProvider => PluginProviderErrorCode::UnknownProvider,
            ProviderErrorCode::StaleGeneration => PluginProviderErrorCode::StaleGeneration,
            ProviderErrorCode::StaleRevision => PluginProviderErrorCode::StaleRevision,
            ProviderErrorCode::Cancelled => PluginProviderErrorCode::Cancelled,
            ProviderErrorCode::Expired => PluginProviderErrorCode::Expired,
            ProviderErrorCode::InvalidSequence => PluginProviderErrorCode::InvalidSequence,
            ProviderErrorCode::DuplicateItem => PluginProviderErrorCode::DuplicateItem,
            ProviderErrorCode::ItemCeiling => PluginProviderErrorCode::ItemCeiling,
            ProviderErrorCode::ByteCeiling => PluginProviderErrorCode::ByteCeiling,
            ProviderErrorCode::Malformed => PluginProviderErrorCode::Malformed,
            ProviderErrorCode::Unavailable => PluginProviderErrorCode::Unavailable,
            ProviderErrorCode::Internal => PluginProviderErrorCode::Internal,
        },
        error.detail,
    )
}

pub(super) fn component_provider_error(
    error: PluginProviderError,
) -> component_api::dust::plugin::host::ProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    component_api::dust::plugin::host::ProviderError {
        code: match error.code {
            PluginProviderErrorCode::QueueFull => ProviderErrorCode::QueueFull,
            PluginProviderErrorCode::UnknownRequest => ProviderErrorCode::UnknownRequest,
            PluginProviderErrorCode::UnknownProvider => ProviderErrorCode::UnknownProvider,
            PluginProviderErrorCode::StaleGeneration => ProviderErrorCode::StaleGeneration,
            PluginProviderErrorCode::StaleRevision => ProviderErrorCode::StaleRevision,
            PluginProviderErrorCode::Cancelled => ProviderErrorCode::Cancelled,
            PluginProviderErrorCode::Expired => ProviderErrorCode::Expired,
            PluginProviderErrorCode::InvalidSequence => ProviderErrorCode::InvalidSequence,
            PluginProviderErrorCode::DuplicateItem => ProviderErrorCode::DuplicateItem,
            PluginProviderErrorCode::ItemCeiling => ProviderErrorCode::ItemCeiling,
            PluginProviderErrorCode::ByteCeiling => ProviderErrorCode::ByteCeiling,
            PluginProviderErrorCode::Malformed => ProviderErrorCode::Malformed,
            PluginProviderErrorCode::Unavailable => ProviderErrorCode::Unavailable,
            PluginProviderErrorCode::Internal => ProviderErrorCode::Internal,
        },
        detail: error.detail,
    }
}

pub(super) fn convert_component_storage_request(
    request: component_api::dust::plugin::host::StorageRequest,
) -> PluginStorageRequestKind {
    use component_api::dust::plugin::host::StorageRequest;
    match request {
        StorageRequest::Get(key) => PluginStorageRequestKind::Get { key },
        StorageRequest::CompareAndSet(request) => PluginStorageRequestKind::CompareAndSet {
            key: request.key,
            expected_revision: request.expected_revision,
            value: request.value,
        },
        StorageRequest::Delete(request) => PluginStorageRequestKind::Delete {
            key: request.key,
            expected_revision: request.expected_revision,
        },
        StorageRequest::PrefixList(request) => PluginStorageRequestKind::List {
            prefix: request.prefix,
            maximum: request.maximum,
        },
    }
}

pub(super) fn component_storage_error(
    error: PluginStorageError,
) -> component_api::dust::plugin::host::StorageError {
    use component_api::dust::plugin::host::StorageErrorCode;
    component_api::dust::plugin::host::StorageError {
        code: match error.code {
            PluginStorageErrorCode::QueueFull => StorageErrorCode::QueueFull,
            PluginStorageErrorCode::InvalidKey => StorageErrorCode::InvalidKey,
            PluginStorageErrorCode::InvalidValue => StorageErrorCode::InvalidValue,
            PluginStorageErrorCode::StaleRevision => StorageErrorCode::StaleRevision,
            PluginStorageErrorCode::QuotaExceeded => StorageErrorCode::QuotaExceeded,
            PluginStorageErrorCode::Unavailable => StorageErrorCode::Unavailable,
            PluginStorageErrorCode::Corrupt => StorageErrorCode::Corrupt,
            PluginStorageErrorCode::GenerationLost => StorageErrorCode::GenerationLost,
        },
        detail: error.detail,
    }
}

pub(super) fn component_storage_result(
    result: PluginStorageResult,
) -> component_api::dust::plugin::host::StorageResult {
    component_api::dust::plugin::host::StorageResult {
        token: result.token,
        value: result.value,
        revision: result.revision,
        entries: result
            .entries
            .into_iter()
            .map(|entry| component_api::dust::plugin::host::StorageEntry {
                key: entry.key,
                revision: entry.revision,
                value: entry.value,
            })
            .collect(),
        truncated: result.truncated,
        error: result.error.map(component_storage_error),
    }
}

pub(super) fn component_ui_error(
    error: PluginUiError,
) -> component_api::dust::plugin::host::UiError {
    use component_api::dust::plugin::host::UiErrorCode;
    let code = match error.code {
        PluginUiErrorCode::QueueFull => UiErrorCode::QueueFull,
        PluginUiErrorCode::StaleGraph => UiErrorCode::StaleGraph,
        PluginUiErrorCode::StaleTree => UiErrorCode::StaleTree,
        PluginUiErrorCode::StaleHandle => UiErrorCode::StaleHandle,
        PluginUiErrorCode::ForeignGeneration => UiErrorCode::ForeignGeneration,
        PluginUiErrorCode::UndeclaredContent => UiErrorCode::UndeclaredContent,
        PluginUiErrorCode::UndeclaredAction => UiErrorCode::UndeclaredAction,
        PluginUiErrorCode::DisallowedPlacement => UiErrorCode::DisallowedPlacement,
        PluginUiErrorCode::HiddenFocus => UiErrorCode::HiddenFocus,
        PluginUiErrorCode::CrossTab => UiErrorCode::CrossTab,
        PluginUiErrorCode::InstanceCeiling => UiErrorCode::InstanceCeiling,
        PluginUiErrorCode::GraphCeiling => UiErrorCode::GraphCeiling,
        PluginUiErrorCode::InvalidTree => UiErrorCode::InvalidTree,
        PluginUiErrorCode::InvalidPlacement => UiErrorCode::InvalidPlacement,
        PluginUiErrorCode::CloseOrder => UiErrorCode::CloseOrder,
        PluginUiErrorCode::GenerationLost => UiErrorCode::GenerationLost,
        PluginUiErrorCode::InternalRejected => UiErrorCode::InternalRejected,
        PluginUiErrorCode::WrongPresenter => UiErrorCode::WrongPresenter,
        PluginUiErrorCode::StaleSurface => UiErrorCode::StaleSurface,
        PluginUiErrorCode::StaleDimensions => UiErrorCode::StaleDimensions,
        PluginUiErrorCode::InvalidLocalBounds => UiErrorCode::InvalidLocalBounds,
        PluginUiErrorCode::CellOverlap => UiErrorCode::CellOverlap,
        PluginUiErrorCode::SplitWideGrapheme => UiErrorCode::SplitWideGrapheme,
        PluginUiErrorCode::InvalidGrapheme => UiErrorCode::InvalidGrapheme,
        PluginUiErrorCode::InvalidStyle => UiErrorCode::InvalidStyle,
        PluginUiErrorCode::InvalidCursor => UiErrorCode::InvalidCursor,
        PluginUiErrorCode::InvalidHitRegion => UiErrorCode::InvalidHitRegion,
        PluginUiErrorCode::PatchCeiling => UiErrorCode::PatchCeiling,
    };
    component_api::dust::plugin::host::UiError {
        code,
        detail: error.detail,
    }
}

pub(super) fn convert_component_tree(
    tree: component_api::dust::plugin::host::UiTree,
) -> Result<PluginUiTree, PluginUiError> {
    if tree.nodes.len() > 256 {
        return Err(PluginUiError::new(
            PluginUiErrorCode::InvalidTree,
            "retained UI node ceiling exceeded",
        ));
    }
    let nodes = tree
        .nodes
        .into_iter()
        .map(|node| PluginUiNode {
            id: node.id,
            kind: match node.kind {
                component_api::dust::plugin::host::UiNodeKind::Container => {
                    PluginUiNodeKind::Container
                }
                component_api::dust::plugin::host::UiNodeKind::Text => PluginUiNodeKind::Text,
                component_api::dust::plugin::host::UiNodeKind::ListNode => PluginUiNodeKind::List,
                component_api::dust::plugin::host::UiNodeKind::Button => PluginUiNodeKind::Button,
            },
            text: node.text,
            style: match node.style {
                component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
                component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
                component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
                component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
                component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
                component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
                component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
            },
            action: node.action,
            children: node.children,
        })
        .collect();
    Ok(PluginUiTree {
        revision: tree.revision,
        root: tree.root,
        nodes,
    })
}

pub(super) fn convert_component_placement(
    placement: component_api::dust::plugin::host::WindowPlacement,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    size: u16,
) -> PluginRequestedPlacement {
    use component_api::dust::plugin::host::WindowPlacement;
    match placement {
        WindowPlacement::DockedLeft => PluginRequestedPlacement::DockedLeft { size },
        WindowPlacement::DockedRight => PluginRequestedPlacement::DockedRight { size },
        WindowPlacement::DockedTop => PluginRequestedPlacement::DockedTop { size },
        WindowPlacement::DockedBottom => PluginRequestedPlacement::DockedBottom { size },
        WindowPlacement::Floating => PluginRequestedPlacement::Floating {
            x,
            y,
            width,
            height,
        },
        WindowPlacement::Fullscreen => PluginRequestedPlacement::Fullscreen,
        WindowPlacement::Hidden => PluginRequestedPlacement::Hidden,
    }
}

pub(super) fn convert_private_style(
    style: component_api::dust::plugin::host::PrivateStyle,
) -> PluginPrivateStyle {
    PluginPrivateStyle {
        id: style.id,
        style: match style.style {
            component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
            component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
            component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
            component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
            component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
            component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
            component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
        },
    }
}

pub(super) fn convert_private_cell(
    cell: component_api::dust::plugin::host::PrivateCell,
) -> PluginPrivateCell {
    PluginPrivateCell {
        x: cell.x,
        y: cell.y,
        grapheme: cell.grapheme,
        continuation: cell.continuation,
        style: cell.style,
    }
}

pub(super) fn convert_private_hit(
    hit: component_api::dust::plugin::host::PrivateHitRegion,
) -> PluginPrivateHitRegion {
    PluginPrivateHitRegion {
        x: hit.x,
        y: hit.y,
        width: hit.width,
        height: hit.height,
        action: hit.action,
    }
}

pub(super) fn convert_private_snapshot(
    snapshot: component_api::dust::plugin::host::PrivateSnapshot,
) -> PluginPrivateSnapshot {
    PluginPrivateSnapshot {
        revision: snapshot.revision,
        dimensions_revision: snapshot.dimensions_revision,
        styles: snapshot
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: snapshot
            .cells
            .into_iter()
            .map(convert_private_cell)
            .collect(),
        cursor: snapshot.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        hit_regions: snapshot
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

pub(super) fn convert_private_patch(
    patch: component_api::dust::plugin::host::PrivatePatch,
) -> PluginPrivatePatch {
    PluginPrivatePatch {
        revision: patch.revision,
        expected_surface_revision: patch.expected_surface_revision,
        dimensions_revision: patch.dimensions_revision,
        dirty: patch
            .dirty
            .into_iter()
            .map(|rect| PluginPrivateRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
        styles: patch
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: patch.cells.into_iter().map(convert_private_cell).collect(),
        replace_cursor: patch.replace_cursor,
        cursor: patch.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        replace_hit_regions: patch.replace_hit_regions,
        hit_regions: patch
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

pub(super) fn convert_component_ui_request(
    request: component_api::dust::plugin::host::UiRequest,
    declared_content_types: &BTreeSet<String>,
) -> Result<PluginUiRequestKind, PluginUiError> {
    use component_api::dust::plugin::host::UiRequest;
    Ok(match request {
        UiRequest::OpenWindow(request) => {
            if !declared_content_types.contains(&request.content_type) {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::UndeclaredContent,
                    "content type was not statically declared",
                ));
            }
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision: request.expected_graph_revision,
                content_type: request.content_type,
                title: request.title,
                placement: convert_component_placement(
                    request.placement,
                    request.x,
                    request.y,
                    request.width,
                    request.height,
                    request.size,
                ),
                tree: request.tree.map(convert_component_tree).transpose()?,
            }
        }
        UiRequest::ReplaceUi(request) => PluginUiRequestKind::ReplaceUi {
            handle: request.handle,
            expected_tree_revision: request.expected_tree_revision,
            tree: convert_component_tree(request.tree)?,
        },
        UiRequest::PlaceWindow(request) => PluginUiRequestKind::PlaceWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
            placement: convert_component_placement(
                request.placement,
                request.x,
                request.y,
                request.width,
                request.height,
                request.size,
            ),
        },
        UiRequest::FocusWindow(request) => PluginUiRequestKind::FocusWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::CloseWindow(request) => PluginUiRequestKind::CloseWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::ReplacePrivate(request) => {
            if request.snapshot.cells.len() > usize::from(MAXIMUM_PRIVATE_CELLS_PER_CONTENT)
                || request.snapshot.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.snapshot.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .snapshot
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(
                        request
                            .snapshot
                            .hit_regions
                            .iter()
                            .map(|hit| hit.action.len()),
                    )
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchCeiling,
                    "private snapshot exceeds absolute host ceilings",
                ));
            }
            PluginUiRequestKind::ReplacePrivate {
                handle: request.handle,
                snapshot: convert_private_snapshot(request.snapshot),
            }
        }
        UiRequest::PatchPrivate(request) => {
            if request.patch.cells.len() > usize::from(MAXIMUM_PRIVATE_PATCH_CELLS)
                || request.patch.dirty.len() > usize::from(MAXIMUM_PRIVATE_DIRTY_RECTANGLES)
                || request.patch.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.patch.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .patch
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(request.patch.hit_regions.iter().map(|hit| hit.action.len()))
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchCeiling,
                    "private patch exceeds absolute host ceilings",
                ));
            }
            PluginUiRequestKind::PatchPrivate {
                handle: request.handle,
                patch: convert_private_patch(request.patch),
            }
        }
    })
}
