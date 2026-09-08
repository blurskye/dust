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
pub use manifest::*;
mod execution;
pub use execution::*;
mod manager;
pub use manager::*;
pub mod component;
mod index;
pub use component::*;
pub use index::*;

#[cfg(test)]
mod manager_tests;

#[cfg(test)]
pub(crate) use manager_tests::{ExecutionFixture, empty_execution_request};
