//! Workspace edit proposals and validation.

use crate::editor::DocumentSnapshot;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use thiserror::Error;

use crate::editor::text::BufferRevision;

use crate::editor::text::TextBuffer;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;

use crate::workspace::ABSOLUTE_PROPOSAL_EDIT_LIMIT;

use crate::workspace::ABSOLUTE_PROPOSAL_FILE_LIMIT;

use crate::workspace::root::ExecutionSource;

use crate::workspace::RootId;

use crate::workspace::root::WorkspaceState;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProposalId(pub u64);

#[derive(Clone, Debug)]
pub struct WorkspaceEditDraft {
    pub canonical_path: PathBuf,
    pub expected_revision: Option<BufferRevision>,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposedFileEdit {
    pub canonical_path: PathBuf,
    pub base_revision: Option<BufferRevision>,
    pub original_bytes: Vec<u8>,
    pub proposed_bytes: Vec<u8>,
    pub selected: bool,
    pub edit_count: usize,
    pub edits: Vec<TextEdit>,
    pub(crate) preferred_line_ending: crate::editor::text::LineEnding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalState {
    Preview,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug)]
pub struct WorkspaceEditProposal {
    pub id: ProposalId,
    pub label: String,
    pub files: Vec<ProposedFileEdit>,
    pub state: ProposalState,
}

impl WorkspaceEditProposal {
    pub fn build(
        id: ProposalId,
        label: String,
        workspace: &WorkspaceState,
        drafts: Vec<WorkspaceEditDraft>,
        snapshots: Vec<DocumentSnapshot>,
    ) -> Result<Self, WorkspaceError> {
        if drafts.len() > ABSOLUTE_PROPOSAL_FILE_LIMIT {
            return Err(WorkspaceError::ProposalFileLimit);
        }
        let edit_count = drafts.iter().map(|draft| draft.edits.len()).sum::<usize>();
        if edit_count > ABSOLUTE_PROPOSAL_EDIT_LIMIT {
            return Err(WorkspaceError::ProposalEditLimit);
        }
        let snapshots = snapshots
            .into_iter()
            .map(|snapshot| (snapshot.canonical_path.clone(), snapshot))
            .collect::<HashMap<_, _>>();
        let mut files = Vec::with_capacity(drafts.len());
        let mut seen = HashSet::new();
        for draft in drafts {
            workspace.require_path(&draft.canonical_path)?;
            if !seen.insert(draft.canonical_path.clone()) {
                return Err(WorkspaceError::DuplicateProposalTarget(
                    draft.canonical_path,
                ));
            }
            let snapshot = snapshots
                .get(&draft.canonical_path)
                .ok_or_else(|| WorkspaceError::MissingSnapshot(draft.canonical_path.clone()))?;
            if draft.expected_revision != snapshot.revision {
                return Err(WorkspaceError::StaleProposal {
                    path: draft.canonical_path,
                    expected: draft.expected_revision,
                    actual: snapshot.revision,
                });
            }
            let mut text = TextBuffer::from_bytes(&snapshot.bytes)
                .map_err(|error| WorkspaceError::InvalidText(error.to_string()))?;
            let base_revision = text.revision();
            let edit_set =
                TextEditSet::new(base_revision, draft.edits.clone()).with_normalized_line_endings();
            text.validate_edits(&edit_set)
                .map_err(|error| WorkspaceError::InvalidEdit(error.to_string()))?;
            text.apply_edits(&edit_set)
                .map_err(|error| WorkspaceError::InvalidEdit(error.to_string()))?;
            files.push(ProposedFileEdit {
                canonical_path: snapshot.canonical_path.clone(),
                base_revision: snapshot.revision,
                original_bytes: snapshot.bytes.clone(),
                proposed_bytes: text.to_bytes(),
                selected: true,
                edit_count: draft.edits.len(),
                edits: draft.edits,
                preferred_line_ending: text.format().preferred_line_ending,
            });
        }
        Ok(Self {
            id,
            label,
            files,
            state: ProposalState::Preview,
        })
    }

    pub fn toggle_file(&mut self, index: usize) -> Result<(), WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        let file = self
            .files
            .get_mut(index)
            .ok_or(WorkspaceError::UnknownProposalFile(index))?;
        file.selected = !file.selected;
        Ok(())
    }

    pub fn accept(&mut self) -> Result<Vec<ProposedFileEdit>, WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        self.state = ProposalState::Accepted;
        Ok(self
            .files
            .iter()
            .filter(|file| file.selected)
            .cloned()
            .collect())
    }

    pub fn reject(&mut self) -> Result<(), WorkspaceError> {
        if self.state != ProposalState::Preview {
            return Err(WorkspaceError::ProposalClosed);
        }
        self.state = ProposalState::Rejected;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace has no roots")]
    NoRoots,
    #[error("workspace has more than {0} roots")]
    TooManyRoots(usize),
    #[error("workspace root name is empty")]
    EmptyRootName,
    #[error("workspace root is duplicated: {0}")]
    DuplicateRoot(PathBuf),
    #[error("workspace root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("unknown workspace root {0:?}")]
    UnknownRoot(RootId),
    #[error("workspace must retain at least one root")]
    LastRoot,
    #[error("path is outside every workspace root: {0}")]
    PathOutsideRoots(PathBuf),
    #[error("{} need a trusted workspace; run :trust", .0.label())]
    TrustRequired(ExecutionSource),
    #[error("invalid diagnostic owner")]
    InvalidDiagnosticOwner,
    #[error("diagnostics are stale: current {expected:?}, result {actual:?}")]
    StaleDiagnostics {
        expected: Option<BufferRevision>,
        actual: Option<BufferRevision>,
    },
    #[error("diagnostic replacement mixes URI or version ownership")]
    MixedDiagnosticSet,
    #[error("workspace edit exceeds the file safety limit")]
    ProposalFileLimit,
    #[error("workspace edit exceeds the edit safety limit")]
    ProposalEditLimit,
    #[error("workspace edit contains duplicate target {0}")]
    DuplicateProposalTarget(PathBuf),
    #[error("workspace edit has no captured snapshot for {0}")]
    MissingSnapshot(PathBuf),
    #[error("workspace edit for {path} is stale: expected {expected:?}, found {actual:?}")]
    StaleProposal {
        path: PathBuf,
        expected: Option<BufferRevision>,
        actual: Option<BufferRevision>,
    },
    #[error("workspace edit text is invalid: {0}")]
    InvalidText(String),
    #[error("workspace edit is invalid: {0}")]
    InvalidEdit(String),
    #[error("workspace edit proposal is already closed")]
    ProposalClosed,
    #[error("workspace edit has no file at index {0}")]
    UnknownProposalFile(usize),
    #[error("cannot {operation} `{}`: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
