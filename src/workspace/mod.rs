#![forbid(unsafe_code)]

pub mod git;
pub mod search_workflow;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet,
};

const ABSOLUTE_ROOT_LIMIT: usize = 64;
const ABSOLUTE_DIAGNOSTIC_LIMIT: usize = 100_000;
const ABSOLUTE_PICKER_ITEM_LIMIT: usize = 100_000;
const ABSOLUTE_PROPOSAL_FILE_LIMIT: usize = 1_024;
const ABSOLUTE_PROPOSAL_EDIT_LIMIT: usize = 100_000;
mod root;
pub use root::{
    ExecutionSource, RootId, RootIdentity, WorkspaceId, WorkspaceRoot, WorkspaceState,
    WorkspaceTrust,
};
mod trust;
pub use trust::{
    TRUST_STATE_VERSION, TrustDatabase, TrustEvent, TrustStateError, TrustStore,
    WorkspaceTrustService,
};
mod explorer;
pub use explorer::{
    ExplorerChild, ExplorerKind, ExplorerLoadState, ExplorerNode, ExplorerNodeId, ExplorerState,
};
mod diagnostics;
pub(crate) use diagnostics::DiagnosticProducer;
pub use diagnostics::{
    Diagnostic, DiagnosticOwner, DiagnosticSeverity, DiagnosticStore, ProblemRow, RelatedDiagnostic,
};
mod picker;
pub use picker::{PickerAction, PickerItem, PickerItemId, PickerPresentation, PickerState};
mod edit;
pub use edit::{
    FileSnapshot, ProposalId, ProposalState, ProposedFileEdit, WorkspaceEditDraft,
    WorkspaceEditProposal, WorkspaceError,
};

#[cfg(test)]
mod tests;
