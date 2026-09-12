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

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::Duration,
};
use thiserror::Error;
mod manifest;
pub(crate) use manifest::PluginCatalogVersion;
use manifest::{
    MAXIMUM_PLUGIN_EVENT_QUEUE, MAXIMUM_PLUGIN_PROVIDERS, MAXIMUM_PLUGIN_SUBSCRIPTIONS,
    MAXIMUM_PLUGIN_VIRTUAL_DOCUMENTS, MAXIMUM_PRIVATE_BYTES, MAXIMUM_PRIVATE_CELLS_PER_CONTENT,
    MAXIMUM_PRIVATE_CELLS_PER_PLUGIN, MAXIMUM_PRIVATE_DIRTY_RECTANGLES,
    MAXIMUM_PRIVATE_HIT_REGIONS, MAXIMUM_PRIVATE_PATCH_CELLS, MAXIMUM_PRIVATE_STYLES,
    MAXIMUM_PROVIDER_BYTES, MAXIMUM_PROVIDER_ITEMS, MAXIMUM_STORAGE_ITEMS,
    MAXIMUM_STORAGE_TOTAL_BYTES, MAXIMUM_STORAGE_VALUE_BYTES,
    MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES, MAXIMUM_VIRTUAL_DOCUMENT_BYTES,
    MAXIMUM_VIRTUAL_DOCUMENT_LINES, MAXIMUM_VIRTUAL_PATCH_BYTES, PLUGIN_UI_HOST_COMMANDS,
};
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
pub(crate) use execution::{ActiveRuntime, LoadOutcome};
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
use component::{
    component_confirmation_result, component_provider_request, component_storage_result,
    component_ui_error, component_virtual_document_request,
};
pub use index::{PluginError, activate_candidate, index_manifest_contributions};
pub(crate) use index::{execute_component, load_component};
use index::{index_manifest_from_store, load_manifest_component, validate_manifest};

#[cfg(test)]
mod manager_tests;
#[cfg(test)]
pub(crate) mod target03;

#[cfg(test)]
pub(crate) use manager_tests::{ExecutionFixture, empty_execution_request};
