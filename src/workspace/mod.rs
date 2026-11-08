//! Workspace roots, diagnostics and picker data.

#![forbid(unsafe_code)]

const ABSOLUTE_ROOT_LIMIT: usize = 64;
const ABSOLUTE_DIAGNOSTIC_LIMIT: usize = 100_000;
pub(crate) const ABSOLUTE_PICKER_ITEM_LIMIT: usize = 100_000;
const ABSOLUTE_PROPOSAL_FILE_LIMIT: usize = 1_024;
const ABSOLUTE_PROPOSAL_EDIT_LIMIT: usize = 100_000;
mod root;
pub use root::ExecutionSource;
pub use root::RootId;
pub use root::WorkspaceId;
pub use root::WorkspaceRoot;
pub use root::WorkspaceState;
pub use root::WorkspaceTrust;
mod trust;
pub use trust::TRUST_STATE_VERSION;
pub use trust::TrustDatabase;
pub use trust::TrustEvent;
pub use trust::TrustStateError;
pub use trust::TrustStore;
pub use trust::WorkspaceTrustWorker;
mod diagnostics;
pub use diagnostics::Diagnostic;
pub use diagnostics::DiagnosticOwner;
pub(crate) use diagnostics::DiagnosticProducer;
pub use diagnostics::DiagnosticSeverity;
pub use diagnostics::DiagnosticStore;
pub use diagnostics::ProblemRow;
pub use diagnostics::RelatedDiagnostic;
mod picker;
pub use picker::PickerAction;
pub use picker::PickerItem;
pub use picker::PickerItemId;
pub use picker::PickerItemKind;
pub use picker::PickerPresentation;
pub use picker::PickerRowPresentation;
pub use picker::PickerState;
pub use picker::fuzzy_score;
mod edit;
pub use edit::ProposalId;
pub use edit::ProposalState;
pub use edit::ProposedFileEdit;
pub use edit::WorkspaceEditDraft;
pub use edit::WorkspaceEditProposal;
pub use edit::WorkspaceError;

#[cfg(test)]
mod tests;
