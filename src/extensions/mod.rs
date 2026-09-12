#![forbid(unsafe_code)]

mod providers;
mod storage;
mod virtual_documents;
pub use providers::{
    PROVIDER_ACTION_BYTE_CEILING, PROVIDER_CHUNK_ITEM_CEILING, PROVIDER_CONTEXT_BYTE_CEILING,
    PROVIDER_ITEM_ID_BYTE_CEILING, PROVIDER_PRIMARY_BYTE_CEILING, PROVIDER_QUERY_BYTE_CEILING,
    PROVIDER_REQUEST_QUEUE_CEILING, PROVIDER_SECONDARY_BYTE_CEILING, PluginPickerChunk,
    PluginPickerItem, PluginPickerProjection, PluginPickerRequestEnvelope, PluginProviderBroker,
    PluginProviderError, PluginProviderErrorCode, PluginProviderSessionStatus, PluginProviderStart,
    PluginProviderTerminal,
};
pub use storage::{
    PluginStorageEntry, PluginStorageError, PluginStorageErrorCode, PluginStorageRequest,
    PluginStorageRequestKind, PluginStorageResult, PluginStorageService, STORAGE_KEY_BYTE_CEILING,
    STORAGE_LIST_BYTE_CEILING, STORAGE_LIST_ENTRY_CEILING, STORAGE_REQUEST_CEILING,
    STORAGE_RESULT_QUEUE_BYTE_CEILING, STORAGE_VALUE_BYTE_CEILING,
};
pub(crate) use virtual_documents::PluginVirtualRequestLane;
pub use virtual_documents::{
    PluginVirtualDocumentBroker, PluginVirtualDocumentError, PluginVirtualDocumentErrorCode,
    PluginVirtualDocumentFull, PluginVirtualDocumentPatch, PluginVirtualDocumentPatchSet,
    PluginVirtualDocumentProposal, PluginVirtualDocumentProposalKind, PluginVirtualDocumentRequest,
    PluginVirtualDocumentRequestKind, PluginVirtualDocumentSnapshot,
    VIRTUAL_DOCUMENT_KEY_BYTE_CEILING, VIRTUAL_DOCUMENT_LANGUAGE_BYTE_CEILING,
    VIRTUAL_DOCUMENT_REQUEST_QUEUE_CEILING, VIRTUAL_DOCUMENT_TITLE_BYTE_CEILING,
};

mod manifest;
pub(crate) use manifest::PluginCatalogVersion;
pub use manifest::{
    PluginActivationBehavior, PluginArgumentDeclaration, PluginArgumentKind, PluginCapability,
    PluginCommandContext, PluginCommandDeclaration, PluginContentTypeContribution,
    PluginContributionCatalog, PluginContributionIndex, PluginContributionProvenance,
    PluginContributions, PluginInputContextContribution, PluginKeyGroupContribution,
    PluginKeymapContribution, PluginLimits, PluginManifest, PluginPresenterKind,
    PluginPrivateCellPolicy, PluginProviderContribution, PluginProviderKind,
    PluginStorageContribution, PluginSubscriptionContribution, PluginSubscriptionKind,
    PluginVirtualDocumentContribution, PluginVirtualDocumentMode, PluginWindowPlacementKind,
};
mod execution;
pub use execution::{
    LoadedComponent, PluginConfirmationAnswer, PluginConfirmationError,
    PluginConfirmationErrorCode, PluginConfirmationOutcome, PluginConfirmationRequest,
    PluginConfirmationResult, PluginEdit, PluginEditProposal, PluginExecution,
    PluginExecutionRequest, PluginHostEventEnvelope, PluginHostEventQueue, PluginLoadId,
    PluginPrivateCell, PluginPrivateCursor, PluginPrivateHitRegion, PluginPrivatePatch,
    PluginPrivateRect, PluginPrivateSnapshot, PluginPrivateStyle, PluginRequestedPlacement,
    PluginSnapshot, PluginStatus, PluginSurfaceGeometry, PluginUiError, PluginUiErrorCode,
    PluginUiEvent, PluginUiNode, PluginUiNodeKind, PluginUiRequest, PluginUiRequestKind,
    PluginUiStyle, PluginUiTree,
};
mod manager;
pub(crate) use manager::{
    OwnedPluginEvent, PluginExecutionReply, PluginInvocation, PluginInvocationOrigin,
};
pub use manager::{PluginEvent, PluginManager};
pub mod component;
mod index;
pub use component::{ComponentHostState, component_api};
pub use index::{PluginError, activate_candidate, index_manifest_contributions};
#[cfg(test)]
pub(crate) use index::{execute_component, load_component};

#[cfg(test)]
mod manager_tests;
#[cfg(test)]
pub(crate) mod target03;

#[cfg(test)]
pub(crate) use manager_tests::{ExecutionFixture, empty_execution_request};
