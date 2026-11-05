//! Protocol-neutral workspace edit preparation, approval and resource lifetimes.
mod preparation;
use super::workspace::WorkspaceVersion;
use crate::{
    editor::transaction::{
        BufferPathChange, EditorDraft, EditorPublication, WorkspaceCommitCheck,
        WorkspaceEditSources,
    },
    files::{CheckedResourceIntent, RequestId, ResourceIntentKind},
    workspace::{
        FileSnapshot, ProposalId, WorkspaceEditDraft, WorkspaceEditProposal, WorkspaceState,
    },
};
use preparation::Preparation;
pub(super) use preparation::{InputFile, IoPermit, PreparationTicket, PreparedFiles};
use std::{collections::HashMap, path::Path, sync::Arc};

#[derive(Clone, Debug)]
pub(super) struct OriginKey(Arc<()>);
impl OriginKey {
    pub(super) fn new() -> Self {
        Self(Arc::new(()))
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
pub(super) struct PreparedEdits {
    pub ticket: PreparationTicket,
    pub label: String,
    pub drafts: Vec<WorkspaceEditDraft>,
    pub snapshots: Vec<FileSnapshot>,
    pub resources: Vec<CheckedResourceIntent>,
}
struct Source {
    id: ProposalId,
    key: Arc<()>,
    workspace: WorkspaceVersion,
    configuration: u64,
    editor: WorkspaceEditSources,
}
impl Source {
    fn current(&self, workspace: &WorkspaceVersion, configuration: u64) -> bool {
        &self.workspace == workspace && self.configuration == configuration
    }
}
struct Preview {
    source: Source,
    proposal: WorkspaceEditProposal,
    resources: Vec<CheckedResourceIntent>,
    origin: Option<OriginKey>,
    all_files: bool,
}
struct Approval {
    key: Arc<()>,
    check: WorkspaceCommitCheck,
    changes: Vec<BufferPathChange>,
}
struct PendingApproval {
    worker: std::thread::JoinHandle<Result<EditorDraft, String>>,
    changes: Vec<BufferPathChange>,
}
pub(super) struct ResourceApplication {
    proposal: ProposalId,
    intent: CheckedResourceIntent,
    target: Option<std::path::PathBuf>,
    change: BufferPathChange,
    workspace: WorkspaceVersion,
    configuration: u64,
    retired: Option<String>,
}
impl ResourceApplication {
    pub(super) fn intent(&self) -> &CheckedResourceIntent {
        &self.intent
    }
    pub(super) fn change(&self) -> &BufferPathChange {
        &self.change
    }
    pub(super) fn target(&self) -> Option<&Path> {
        self.target.as_deref()
    }
    pub(super) fn into_change(self) -> (BufferPathChange, Option<std::path::PathBuf>) {
        (self.change, self.target)
    }
}
pub(super) struct ResourcePermit<'a> {
    accepted: &'a mut Vec<ResourceApplication>,
    applying: &'a mut HashMap<RequestId, (Arc<()>, ResourceApplication)>,
    _workspace: &'a super::workspace::WorkspaceSession,
    _configuration: &'a u64,
    _editor: &'a crate::editor::EditorState,
}
impl ResourcePermit<'_> {
    pub(super) fn intent(
        &self,
        worker: &Arc<()>,
    ) -> Result<&CheckedResourceIntent, String> {
        if self
            .applying
            .values()
            .any(|(expected, _)| !Arc::ptr_eq(expected, worker))
        {
            return Err("workspace resources already belong to another document worker".into());
        }
        Ok(self.accepted[0].intent())
    }
    pub(super) fn finish(
        self,
        submission: crate::editor::session::documents::WorkspaceSubmission,
    ) -> Result<(), String> {
        let (worker, result) = submission.into_parts();
        let id = result.map_err(|error| {
            format!(
                "proposal {} resource admission failed: {error}",
                self.accepted[0].proposal.0
            )
        })?;
        let request = self.accepted.remove(0);
        match self.applying.entry(id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((worker, request));
                Ok(())
            }
            _ => Err("resource identity reused; admitted disk work may still finish".into()),
        }
    }
}
pub(super) struct Retirement {
    pub requests: Vec<RequestId>,
    pub origin: Option<OriginKey>,
}
pub(super) struct WorkspaceEditSession {
    owner: Arc<()>,
    next: u64,
    preparing: Option<Preparation>,
    preview: Option<Preview>,
    approval: Option<Approval>,
    pending_approval: Option<PendingApproval>,
    accepted: Vec<ResourceApplication>,
    applying: HashMap<RequestId, (Arc<()>, ResourceApplication)>,
}
impl Default for WorkspaceEditSession {
    fn default() -> Self {
        Self {
            owner: Arc::new(()),
            next: 1,
            preparing: None,
            preview: None,
            approval: None,
            pending_approval: None,
            accepted: Vec::new(),
            applying: HashMap::new(),
        }
    }
}
impl WorkspaceEditSession {
    pub(super) fn check_begin(&self) -> Result<(), String> {
        if self.preparing.is_some() || self.preview.is_some() {
            return Err("another workspace edit is pending".into());
        }
        if self.accepted.len() + self.applying.len() >= 1024 {
            return Err("too many staged workspace resources".into());
        }
        self.next
            .checked_add(1)
            .ok_or("workspace proposal identity exhausted")?;
        Ok(())
    }
    pub(super) fn begin(
        &mut self,
        workspace: WorkspaceVersion,
        configuration: u64,
        editor: WorkspaceEditSources,
    ) -> Result<PreparationTicket, String> {
        self.check_begin()?;
        let id = ProposalId(self.next);
        self.next += 1;
        let key = Arc::new(());
        self.preparing = Some(Preparation::new(Source {
            id,
            key: key.clone(),
            workspace,
            configuration,
            editor,
        }));
        Ok(PreparationTicket {
            owner: self.owner.clone(),
            key,
            id,
        })
    }
    fn check_ticket(&self, ticket: &PreparationTicket) -> Result<(), String> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner)
            || self.preparing.as_ref().is_none_or(|p| {
                !Arc::ptr_eq(&p.source.key, &ticket.key) || p.source.id != ticket.id
            })
        {
            return Err("workspace preparation ticket is foreign or retired".into());
        }
        Ok(())
    }
    pub(super) fn current_preparation(
        &self,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        if self
            .preparing
            .as_ref()
            .is_none_or(|p| !p.source.current(workspace, configuration))
        {
            return Err("workspace preparation source retired".into());
        }
        Ok(())
    }
    pub(super) fn build(
        &mut self,
        prepared: PreparedEdits,
        scope: &WorkspaceState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        self.check_ticket(&prepared.ticket)?;
        self.current_preparation(workspace, configuration)?;
        let p = self.preparing.as_mut().expect("checked preparation");
        p.label = prepared.label;
        p.snapshots = prepared.snapshots;
        p.resources = prepared.resources;
        self.finish_preparation(prepared.drafts, scope, workspace, configuration)
    }
    pub(super) fn finish_preparation(
        &mut self,
        mut drafts: Vec<WorkspaceEditDraft>,
        scope: &WorkspaceState,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        self.current_preparation(workspace, configuration)?;
        if !self.files_ready() {
            return Err("workspace preparation has unfinished operations".into());
        }
        let p = self.preparing.as_mut().expect("checked preparation");
        if p.resources.len() + self.accepted.len() + self.applying.len() > 1024 {
            return Err("too many staged workspace resources".into());
        }
        for path in &p.creates {
            if drafts.iter().any(|draft| &draft.canonical_path == path) {
                continue;
            }
            drafts.push(WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: None,
                edits: Vec::new(),
            });
        }
        if p.resources.iter().any(|resource| {
            drafts
                    .iter()
                    .any(|draft| draft.canonical_path == resource.source && !draft.edits.is_empty())
        }) {
            return Err(
                "text edits and renames or deletions of the same file cannot be applied together".into(),
            );
        }
        let proposal = WorkspaceEditProposal::build(
            p.source.id,
            p.label.clone(),
            scope,
            drafts,
            std::mem::take(&mut p.snapshots),
        )
        .map_err(|e| e.to_string())?;
        let p = self.preparing.take().expect("checked preparation");
        self.preview = Some(Preview {
            source: p.source,
            proposal,
            resources: p.resources,
            origin: p.origin,
            all_files: p.all_files,
        });
        Ok(())
    }
    pub(super) fn proposal(&self) -> Option<&WorkspaceEditProposal> {
        self.preview.as_ref().map(|p| &p.proposal)
    }
    pub(super) fn origin(&self) -> Option<&OriginKey> {
        self.preview
            .as_ref()
            .and_then(|p| p.origin.as_ref())
            .or_else(|| self.preparing_origin())
    }
    pub(super) fn resources_pending(&self, proposal: ProposalId) -> bool {
        self.accepted.iter().any(|request| request.proposal == proposal)
            || self.applying.values().any(|(_, request)| request.proposal == proposal)
    }
    pub(super) fn next_resource_proposal(&self) -> Option<ProposalId> {
        self.accepted.first().map(|request| request.proposal)
    }
    pub(super) fn resource_proposal(&self, reply: &super::documents::WorkspaceIoReply) -> Option<ProposalId> {
        self.applying.get(&reply.id())
            .filter(|(worker, _)| reply.is_from(worker))
            .map(|(_, request)| request.proposal)
    }
    pub(super) fn resources(&self) -> &[CheckedResourceIntent] {
        self.preview
            .as_ref()
            .map_or(&[], |p| p.resources.as_slice())
    }
    pub(super) fn toggle(&mut self, index: usize) -> Result<(), String> {
        if self.approval.is_some() || self.pending_approval.is_some() {
            return Err("workspace approval is already staged".into());
        }
        let p = self.preview.as_mut().ok_or("workspace preview is closed")?;
        if p.all_files {
            return Err("requesting source requires the complete edit".into());
        }
        p.proposal.toggle_file(index).map_err(|e| e.to_string())
    }
    pub(super) fn approval(
        &self,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(&WorkspaceEditSources, &WorkspaceEditProposal), String> {
        let p = self.preview.as_ref().ok_or("workspace preview is closed")?;
        if !p.source.current(workspace, configuration) {
            return Err("workspace preview source changed".into());
        }
        Ok((&p.source.editor, &p.proposal))
    }
    pub(super) fn prepare_approval(
        &mut self,
        editor: &crate::editor::EditorState,
        transaction: crate::editor::TransactionId,
        changes: Vec<BufferPathChange>,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<Option<(EditorDraft, Vec<BufferPathChange>)>, String> {
        if self.pending_approval.is_some() || self.approval.is_some() {
            return Err("workspace approval is already staged".into());
        }
        let (sources, proposal) = self.approval(workspace, configuration)?;
        let needs_history = editor.undo_directory.is_some()
            && proposal.files.iter().any(|file| file.selected && file.base_revision.is_none());
        if !needs_history {
            return editor.prepare_workspace_edit(sources, proposal, transaction)
                .map(|draft| Some((draft, changes)));
        }
        // Unopened files restore persistent undo while constructing the draft.
        // Keep that disk access and parsing off the input thread.
        let editor = editor.clone();
        let sources = sources.clone();
        let proposal = proposal.clone();
        let worker = std::thread::Builder::new().name("dust-workspace-edit".into())
            .spawn(move || editor.prepare_workspace_edit(&sources, &proposal, transaction))
            .map_err(|error| error.to_string())?;
        self.pending_approval = Some(PendingApproval { worker, changes });
        Ok(None)
    }
    pub(super) fn poll_approval(
        &mut self,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Option<Result<(EditorDraft, Vec<BufferPathChange>), String>> {
        if !self.pending_approval.as_ref()?.worker.is_finished() {
            return None;
        }
        let pending = self.pending_approval.take()?;
        Some((|| {
            self.approval(workspace, configuration)?;
            let draft = pending.worker.join()
                .map_err(|_| "workspace preparation worker failed".to_owned())??;
            Ok((draft, pending.changes))
        })())
    }
    pub(super) fn stage_approval(
        &mut self,
        draft: &EditorDraft,
        changes: Vec<BufferPathChange>,
    ) -> Result<(), String> {
        let p = self.preview.as_ref().ok_or("workspace preview is closed")?;
        if self.approval.is_some()
            || changes.len() != p.resources.len()
            || changes
                .iter()
                .zip(&p.resources)
                .any(|(change, intent)| change.source() != intent.source)
        {
            return Err("workspace approval did not match the preview".into());
        }
        self.approval = Some(Approval {
            key: p.source.key.clone(),
            check: draft.workspace_commit_check(&p.proposal)?,
            changes,
        });
        Ok(())
    }
    pub(super) fn abandon_approval(&mut self) {
        self.approval = None;
    }
    pub(super) fn accepted(
        &mut self,
        publication: EditorPublication<'_>,
    ) -> Result<Option<OriginKey>, String> {
        let p = self.preview.as_ref().ok_or("workspace preview is closed")?;
        let approval = self
            .approval
            .as_ref()
            .ok_or("workspace approval was not staged")?;
        if !Arc::ptr_eq(&approval.key, &p.source.key) || !approval.check.matches(&publication) {
            return Err("the staged workspace edit was not published".into());
        }
        let p = self.preview.take().expect("checked preview");
        let approval = self.approval.take().expect("checked approval");
        self.accepted
            .extend(
                p.resources
                    .into_iter()
                    .zip(approval.changes)
                    .map(|(intent, change)| {
                        let target = match &intent.kind {
                            ResourceIntentKind::Rename { target, .. } => Some(target.clone()),
                            ResourceIntentKind::Delete => None,
                        };
                        ResourceApplication {
                            proposal: p.source.id,
                            intent,
                            target,
                            change,
                            workspace: p.source.workspace.clone(),
                            configuration: p.source.configuration,
                            retired: None,
                        }
                    }),
            );
        Ok(p.origin)
    }
    pub(super) fn reject(&mut self) -> Retirement {
        self.approval = None;
        // A retired worker only owns a private draft; it cannot publish an edit.
        self.pending_approval = None;
        let preview = self.preview.take();
        let preparing = self.preparing.take();
        Retirement {
            origin: preview
                .and_then(|p| p.origin)
                .or_else(|| preparing.as_ref().and_then(|p| p.origin.clone())),
            requests: preparing.map_or_else(Vec::new, |p| p.requests.into_keys().collect()),
        }
    }
    pub(super) fn prepare_resource<'a>(
        &'a mut self,
        workspace: &'a super::workspace::WorkspaceSession,
        configuration: &'a u64,
        editor: &'a crate::editor::EditorState,
    ) -> Result<Option<ResourcePermit<'a>>, String> {
        let Some(request) = self.accepted.first() else {
            return Ok(None);
        };
        let check = if request.workspace != workspace.version()
            || request.configuration != *configuration
            || *configuration == u64::MAX
        {
            Err("resource application is foreign, retired or exhausted".into())
        } else {
            editor.validate_resource_change(
                request.change(),
                &request.intent().source_bytes,
                request.target(),
            )
        };
        if let Err(error) = check {
            let request = self.accepted.remove(0);
            return Err(format!(
                "proposal {} resource refused before submission: {error}",
                request.proposal.0
            ));
        }
        Ok(Some(ResourcePermit {
            accepted: &mut self.accepted,
            applying: &mut self.applying,
            _workspace: workspace,
            _configuration: configuration,
            _editor: editor,
        }))
    }
    pub(super) fn resource_finished(
        &mut self,
        reply: super::documents::WorkspaceIoReply,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Option<Result<ResourceApplication, String>> {
        let id = reply.id();
        if !self
            .applying
            .get(&id)
            .is_some_and(|(worker, _)| reply.is_from(worker))
        {
            return None;
        }
        let (_, request) = self.applying.remove(&id).expect("checked worker request");
        let retired = request.retired.clone().or_else(|| {
            (&request.workspace != workspace || request.configuration != configuration)
                .then(|| "source change".to_owned())
        });
        Some(match reply.into_result() {
            crate::files::VfsResult::ResourceApplied { result, .. } => {
                result.map(|()| request).map_err(|error| match retired {
                    Some(reason) => format!("resource failed after {reason}: {error}"),
                    None => error.to_string(),
                })
            }
            _ => Err("wrong workspace resource terminal; disk outcome is unknown".into()),
        })
    }
    pub(super) fn discard_staged_resources(&mut self, reason: &str) -> Vec<String> {
        self.accepted
            .drain(..)
            .map(|request| {
                format!(
                    "proposal {} resource was not submitted: {reason}",
                    request.proposal.0
                )
            })
            .collect()
    }
    pub(super) fn retire_resources(&mut self, reason: &str) -> (Vec<String>, Vec<RequestId>) {
        let notices = self.discard_staged_resources(reason);
        let requests = self
            .applying
            .iter_mut()
            .filter_map(|(id, (_, request))| {
                if request.retired.is_some() {
                    return None;
                }
                request.retired = Some(reason.to_owned());
                Some(*id)
            })
            .collect();
        (notices, requests)
    }
    pub(super) fn resource_cancelled(
        &mut self,
        cancelled: super::documents::WorkspaceCancellation,
    ) -> Option<String> {
        let id = cancelled.id();
        if !self
            .applying
            .get(&id)
            .is_some_and(|(worker, _)| cancelled.is_from(worker))
        {
            return None;
        }
        self.applying.remove(&id).map(|(_, request)| {
            format!(
                "proposal {} queued resource cancelled before execution",
                request.proposal.0
            )
        })
    }
    pub(super) fn finish_resources(&mut self, reason: &str) -> Vec<String> {
        let mut notices = self.discard_staged_resources(reason);
        notices.extend(self.applying.drain().map(|(_, (_, request))| format!("proposal {} resource outcome is unknown after {reason}; admitted disk work may have completed or may still finish", request.proposal.0)));
        notices
    }
}

#[cfg(test)]
mod tests;
