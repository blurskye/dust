//! Protocol-neutral workspace edit preparation, approval and resource lifetimes.
mod preparation;
use crate::app::workspace::WorkspaceVersion;
use crate::editor::BufferPathChange;
use crate::editor::DocumentSnapshot as FileSnapshot;
use crate::editor::EditorDraft;
use crate::editor::EditorPublication;
use crate::editor::WorkspaceCommitCheck;
use crate::editor::WorkspaceEditSources;
use crate::files::CheckedResourceIntent;
use crate::files::RequestId;
use crate::files::ResourceIntentKind;
use crate::workspace::ProposalId;
use crate::workspace::WorkspaceEditDraft;
use crate::workspace::WorkspaceEditProposal;
use crate::workspace::WorkspaceState;
pub(in crate::app) use preparation::InputFile;
use preparation::Preparation;
pub(in crate::app) use preparation::PreparationTicket;
pub(in crate::app) use preparation::PreparedFiles;
pub(in crate::app) use preparation::PreparedIo;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(in crate::app) struct OriginKey(Arc<()>);
impl OriginKey {
    pub(in crate::app) fn new() -> Self {
        Self(Arc::new(()))
    }
    pub(in crate::app) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
pub(in crate::app) struct PreparedEdits {
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
    pub(in crate::app) fn current(&self, workspace: &WorkspaceVersion, configuration: u64) -> bool {
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
pub(in crate::app) struct ResourceApplication {
    proposal: ProposalId,
    intent: CheckedResourceIntent,
    target: Option<std::path::PathBuf>,
    change: BufferPathChange,
    workspace: WorkspaceVersion,
    configuration: u64,
    invalidated: Option<String>,
}
impl ResourceApplication {
    pub(in crate::app) fn intent(&self) -> &CheckedResourceIntent {
        &self.intent
    }
    pub(in crate::app) fn change(&self) -> &BufferPathChange {
        &self.change
    }
    pub(in crate::app) fn target(&self) -> Option<&Path> {
        self.target.as_deref()
    }
    pub(in crate::app) fn into_change(self) -> (BufferPathChange, Option<std::path::PathBuf>) {
        (self.change, self.target)
    }
}
pub(in crate::app) struct PreparedResource<'a> {
    accepted: &'a mut Vec<ResourceApplication>,
    applying: &'a mut HashMap<RequestId, (Arc<()>, ResourceApplication)>,
    _workspace: &'a crate::app::workspace::Workspace,
    _configuration: &'a u64,
    _editor: &'a crate::editor::EditorState,
}
impl PreparedResource<'_> {
    pub(in crate::app) fn intent(
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
    pub(in crate::app) fn finish(
        self,
        submission: crate::app::documents::WorkspaceSubmission,
    ) -> Result<(), String> {
        let (worker, result) = submission.into_parts();
        let id = result.map_err(|error| {
            format!(
                "proposal {} resource submission failed: {error}",
                self.accepted[0].proposal.0
            )
        })?;
        let request = self.accepted.remove(0);
        match self.applying.entry(id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((worker, request));
                Ok(())
            }
            _ => Err("resource identity reused; accepted disk work may still finish".into()),
        }
    }
}
pub(in crate::app) struct RejectedProposal {
    pub requests: Vec<RequestId>,
    pub origin: Option<OriginKey>,
}
pub(in crate::app) struct WorkspaceEdits {
    owner: Arc<()>,
    next: u64,
    preparing: Option<Preparation>,
    preview: Option<Preview>,
    approval: Option<Approval>,
    pending_approval: Option<PendingApproval>,
    accepted: Vec<ResourceApplication>,
    applying: HashMap<RequestId, (Arc<()>, ResourceApplication)>,
}
impl Default for WorkspaceEdits {
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
impl WorkspaceEdits {
    pub(in crate::app) fn check_begin(&self) -> Result<(), String> {
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
    pub(in crate::app) fn begin(
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
    pub(in crate::app) fn check_ticket(&self, ticket: &PreparationTicket) -> Result<(), String> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner)
            || self.preparing.as_ref().is_none_or(|p| {
                !Arc::ptr_eq(&p.source.key, &ticket.key) || p.source.id != ticket.id
            })
        {
            return Err("workspace preparation ticket is foreign or invalidated".into());
        }
        Ok(())
    }
    pub(in crate::app) fn current_preparation(
        &self,
        workspace: &WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        if self
            .preparing
            .as_ref()
            .is_none_or(|p| !p.source.current(workspace, configuration))
        {
            return Err("workspace preparation source invalidated".into());
        }
        Ok(())
    }
    pub(in crate::app) fn build(
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
    pub(in crate::app) fn finish_preparation(
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
                "text edits and renames or deletions of the same file cannot be applied together"
                    .into(),
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
    pub(in crate::app) fn proposal(&self) -> Option<&WorkspaceEditProposal> {
        self.preview.as_ref().map(|p| &p.proposal)
    }
    pub(in crate::app) fn origin(&self) -> Option<&OriginKey> {
        self.preview
            .as_ref()
            .and_then(|p| p.origin.as_ref())
            .or_else(|| self.preparing_origin())
    }
    pub(in crate::app) fn resources_pending(&self, proposal: ProposalId) -> bool {
        self.accepted
            .iter()
            .any(|request| request.proposal == proposal)
            || self
                .applying
                .values()
                .any(|(_, request)| request.proposal == proposal)
    }
    pub(in crate::app) fn next_resource_proposal(&self) -> Option<ProposalId> {
        self.accepted.first().map(|request| request.proposal)
    }
    pub(in crate::app) fn resource_proposal(
        &self,
        reply: &crate::app::documents::WorkspaceIoReply,
    ) -> Option<ProposalId> {
        self.applying
            .get(&reply.id())
            .filter(|(worker, _)| reply.is_from(worker))
            .map(|(_, request)| request.proposal)
    }
    pub(in crate::app) fn resources(&self) -> &[CheckedResourceIntent] {
        self.preview
            .as_ref()
            .map_or(&[], |p| p.resources.as_slice())
    }
    pub(in crate::app) fn toggle(&mut self, index: usize) -> Result<(), String> {
        if self.approval.is_some() || self.pending_approval.is_some() {
            return Err("workspace approval is already staged".into());
        }
        let p = self.preview.as_mut().ok_or("workspace preview is closed")?;
        if p.all_files {
            return Err("requesting source requires the complete edit".into());
        }
        p.proposal.toggle_file(index).map_err(|e| e.to_string())
    }
    pub(in crate::app) fn approval(
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
    pub(in crate::app) fn prepare_approval(
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
            && proposal
                .files
                .iter()
                .any(|file| file.selected && file.base_revision.is_none());
        if !needs_history {
            return crate::app::workspace::edits::prepare_workspace_edit(
                editor,
                sources,
                proposal,
                transaction,
            )
            .map(|draft| Some((draft, changes)));
        }
        // Unopened files restore persistent undo while constructing the draft.
        // Keep that disk access and parsing off the input thread.
        let editor = editor.clone();
        let sources = sources.clone();
        let proposal = proposal.clone();
        let worker = std::thread::Builder::new()
            .name("dust-workspace-edit".into())
            .spawn(move || {
                crate::app::workspace::edits::prepare_workspace_edit(
                    &editor,
                    &sources,
                    &proposal,
                    transaction,
                )
            })
            .map_err(|error| error.to_string())?;
        self.pending_approval = Some(PendingApproval { worker, changes });
        Ok(None)
    }
    pub(in crate::app) fn poll_approval(
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
            let draft = pending
                .worker
                .join()
                .map_err(|_| "workspace preparation worker failed".to_owned())??;
            Ok((draft, pending.changes))
        })())
    }
    pub(in crate::app) fn stage_approval(
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
            check: crate::app::workspace::edits::workspace_commit_check(draft, &p.proposal)?,
            changes,
        });
        Ok(())
    }
    pub(in crate::app) fn abandon_approval(&mut self) {
        self.approval = None;
    }
    pub(in crate::app) fn accepted(
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
                            invalidated: None,
                        }
                    }),
            );
        Ok(p.origin)
    }
    pub(in crate::app) fn reject(&mut self) -> RejectedProposal {
        self.approval = None;
        // An invalidated worker only owns a private draft; it cannot publish an edit.
        self.pending_approval = None;
        let preview = self.preview.take();
        let preparing = self.preparing.take();
        RejectedProposal {
            origin: preview
                .and_then(|p| p.origin)
                .or_else(|| preparing.as_ref().and_then(|p| p.origin.clone())),
            requests: preparing.map_or_else(Vec::new, |p| p.requests.into_keys().collect()),
        }
    }
    pub(in crate::app) fn prepare_resource<'a>(
        &'a mut self,
        workspace: &'a crate::app::workspace::Workspace,
        configuration: &'a u64,
        editor: &'a crate::editor::EditorState,
    ) -> Result<Option<PreparedResource<'a>>, String> {
        let Some(request) = self.accepted.first() else {
            return Ok(None);
        };
        let check = if request.workspace != workspace.version()
            || request.configuration != *configuration
            || *configuration == u64::MAX
        {
            Err("resource application is foreign, invalidated or exhausted".into())
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
        Ok(Some(PreparedResource {
            accepted: &mut self.accepted,
            applying: &mut self.applying,
            _workspace: workspace,
            _configuration: configuration,
            _editor: editor,
        }))
    }
    pub(in crate::app) fn resource_finished(
        &mut self,
        reply: crate::app::documents::WorkspaceIoReply,
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
        let invalidated = request.invalidated.clone().or_else(|| {
            (&request.workspace != workspace || request.configuration != configuration)
                .then(|| "source change".to_owned())
        });
        Some(match reply.into_result() {
            crate::files::VfsResult::ResourceApplied { result, .. } => {
                result.map(|()| request).map_err(|error| match invalidated {
                    Some(reason) => format!("resource failed after {reason}: {error}"),
                    None => error.to_string(),
                })
            }
            _ => Err("wrong workspace resource terminal; disk outcome is unknown".into()),
        })
    }
    pub(in crate::app) fn discard_staged_resources(&mut self, reason: &str) -> Vec<String> {
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
    pub(in crate::app) fn invalidate_resources(
        &mut self,
        reason: &str,
    ) -> (Vec<String>, Vec<RequestId>) {
        let notices = self.discard_staged_resources(reason);
        let requests = self
            .applying
            .iter_mut()
            .filter_map(|(id, (_, request))| {
                if request.invalidated.is_some() {
                    return None;
                }
                request.invalidated = Some(reason.to_owned());
                Some(*id)
            })
            .collect();
        (notices, requests)
    }
    pub(in crate::app) fn resource_cancelled(
        &mut self,
        cancelled: crate::app::documents::WorkspaceCancellation,
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
    pub(in crate::app) fn finish_resources(&mut self, reason: &str) -> Vec<String> {
        let mut notices = self.discard_staged_resources(reason);
        notices.extend(self.applying.drain().map(|(_, (_, request))| format!("proposal {} resource outcome is unknown after {reason}; accepted disk work may have completed or may still finish", request.proposal.0)));
        notices
    }
}

#[cfg(test)]
mod tests;

fn selected_edits(
    proposal: &crate::workspace::WorkspaceEditProposal,
) -> Vec<crate::editor::DraftEdit<'_>> {
    proposal
        .files
        .iter()
        .filter(|file| file.selected)
        .map(|file| crate::editor::DraftEdit {
            canonical_path: &file.canonical_path,
            base_revision: file.base_revision,
            original_bytes: &file.original_bytes,
            edits: &file.edits,
            preferred_line_ending: file.preferred_line_ending,
        })
        .collect()
}
pub(in crate::app) fn prepare_workspace_edit(
    state: &crate::editor::EditorState,
    sources: &crate::editor::WorkspaceEditSources,
    proposal: &crate::workspace::WorkspaceEditProposal,
    transaction: crate::editor::TransactionId,
) -> Result<crate::editor::EditorDraft, String> {
    // Preserve editor identity validation before the preview-state check.
    if !sources.same_editor(state) {
        return Err("workspace edit belongs to a foreign or exhausted editor".into());
    }
    if proposal.state != crate::workspace::ProposalState::Preview {
        return Err("workspace preview is closed".into());
    }
    state.prepare_document_edits(
        sources,
        &proposal.label,
        &selected_edits(proposal),
        transaction,
    )
}
pub(in crate::app) fn workspace_commit_check(
    draft: &crate::editor::EditorDraft,
    proposal: &crate::workspace::WorkspaceEditProposal,
) -> Result<crate::editor::WorkspaceCommitCheck, String> {
    draft.check_document_edits(
        proposal
            .files
            .iter()
            .filter(|file| file.selected)
            .map(|file| {
                (
                    file.canonical_path.as_path(),
                    file.proposed_bytes.as_slice(),
                )
            }),
    )
}

#[cfg(test)]
mod draft_tests;

mod commands;

use crate::app::App;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::editor::text::PositionEncoding;
use crate::files::VfsResult;
use crate::language::lsp::LspWorkspaceEdit;
use crate::ui::PickerSurfaceKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    pub(in crate::app) fn process_workspace_io(
        &mut self,
        reply: crate::app::documents::WorkspaceIoReply,
    ) {
        if self.workspace_edits.owns_preparation_reply(&reply) {
            if let VfsResult::ResourceResolved {
                result: Ok(intent), ..
            } = reply.result()
            {
                let target = match &intent.kind {
                    ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                    ResourceIntentKind::Delete => None,
                };
                if let Err(error) = self.state.check_resource_destination(target) {
                    self.abort_workspace_edit(error);
                    return;
                }
            }
            match self.workspace_edits.accept_preparation_reply(
                reply,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            ) {
                Ok(()) => {
                    if self.workspace_edits.files_ready() {
                        self.finish_workspace_proposal();
                    }
                }
                Err(error) => self.abort_workspace_edit(error),
            }
            return;
        }

        let Some(proposal) = self.workspace_edits.resource_proposal(&reply) else {
            return;
        };
        let Some(result) = self.workspace_edits.resource_finished(
            reply,
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                self.language.cancel_deferred_edits(Some(proposal));
                self.show_error(error);
                return;
            }
        };
        let (change, target) = request.into_change();
        match self.state.reconcile_paths(change, target.as_deref()) {
            Ok((draft, recent)) => {
                if let Some(draft) = draft
                    && let Err(error) = self.commit_editor_draft(draft, None, false)
                {
                    self.language.cancel_deferred_edits(Some(proposal));
                    self.show_error(error);
                    return;
                }
                for path in recent {
                    self.record_recent_file(path);
                }
            }
            Err(error) => {
                self.language.cancel_deferred_edits(Some(proposal));
                self.show_error(error);
                return;
            }
        }
        if !self.workspace_edits.resources_pending(proposal)
            && let Err(error) = self.language.finish_deferred_edit(proposal, &self.state)
        {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_workspace_resources(&mut self, reason: &str) {
        self.language.cancel_deferred_edits(None);
        let (notices, requests) = self.workspace_edits.invalidate_resources(reason);
        for notice in notices {
            self.show_error(notice);
        }
        for id in requests {
            if let Some(cancelled) = self.documents.cancel_workspace_queued(id) {
                if let Some(notice) = self.workspace_edits.resource_cancelled(cancelled) {
                    self.show_error(notice);
                }
            } else {
                self.show_error(
                    "resource publication invalidated; its filesystem outcome is still pending",
                );
            }
        }
    }
}

impl App {
    pub(in crate::app) fn finish_workspace_resources(&mut self, reason: &str) {
        self.language.cancel_deferred_edits(None);
        for notice in self.workspace_edits.finish_resources(reason) {
            self.show_error(notice);
        }
    }
}

impl App {
    pub(in crate::app) fn begin_workspace_proposal(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
    ) {
        self.begin_workspace_proposal_owned(label, edit, response, encoding, None);
    }
}

impl App {
    pub(in crate::app) fn begin_workspace_proposal_owned(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<crate::language::lsp::ApplyEditReply>,
        encoding: PositionEncoding,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        if let Err(error) = self.workspace_edits.check_begin().and_then(|()| {
            self.state
                .window_version()
                .revision()
                .map(|_| ())
                .ok_or_else(|| "editor source revision is exhausted".into())
        }) {
            self.language.reject_reply(response, error.clone());
            self.show_error(error);
            return;
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::app::language::edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                    dirty: buffer.dirty,
                })
            })
            .collect::<Vec<_>>();
        let plan = match self
            .language
            .start_edit(edit, encoding, context, response, &documents)
        {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let ticket = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(ticket) => ticket,
            Err(error) => {
                let _ =
                    self.language
                        .finish_edit(&plan.key, false, Some(error.clone()), &self.state);
                self.show_error(error);
                return;
            }
        };
        let maximum = crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        if let Err(error) = self.workspace_edits.start_files(
            ticket,
            label,
            Some(plan.key),
            plan.all_files,
            plan.files,
            (roots, maximum),
        ) {
            self.abort_workspace_edit(error);
            return;
        }
        self.submit_workspace_preparation();
        if self.workspace_edits.preparing() {
            self.notify_message(self.message("workspace_edit_loading", &[]), false);
        }
    }
}

impl App {
    pub(in crate::app) fn submit_workspace_preparation(&mut self) {
        let maximum = crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
        while let Some((index, file)) = self.workspace_edits.next_file() {
            let snapshot = if let crate::app::workspace::edits::InputFile::Read(path) = file {
                self.state.workspace_snapshot(
                    self.workspace_edits
                        .preparing_sources()
                        .expect("accepted source"),
                    path,
                    maximum,
                )
            } else {
                Ok(None)
            };
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.abort_workspace_edit(error);
                    return;
                }
            };
            if let Some(snapshot) = snapshot {
                if let Err(error) = self.workspace_edits.open_snapshot(index, snapshot) {
                    self.abort_workspace_edit(error);
                    return;
                }
            } else {
                let accepted = self
                    .workspace_edits
                    .prepare_io(
                        index,
                        &self.workspace,
                        self.configuration.binding_revision_ref(),
                        &self.state,
                    )
                    .and_then(|prepared| self.documents.submit_workspace_io(prepared));
                match accepted {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(error) => {
                        self.abort_workspace_edit(error);
                        return;
                    }
                }
            }
        }
        if self.workspace_edits.files_ready() {
            self.finish_workspace_proposal();
        }
    }
}

impl App {
    fn check_workspace_edit_origin(&self) -> Result<(), String> {
        let Some(key) = self.workspace_edits.origin() else {
            return Ok(());
        };
        if self.extension.owns_edit(key) {
            return self.extension.check_edit(
                key,
                &self.state,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            );
        }
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::app::language::edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                    dirty: buffer.dirty,
                })
            })
            .collect::<Vec<_>>();
        self.language.check_edit(key, &documents)
    }
}

impl App {
    pub(in crate::app) fn finish_workspace_proposal(&mut self) {
        let result = (|| {
            self.workspace_edits.current_preparation(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            self.check_workspace_edit_origin()?;
            let source = self
                .workspace_edits
                .preparing_sources()
                .ok_or("workspace preparation source missing")?;
            let files = self.workspace_edits.prepared_files()?;
            self.state
                .validate_workspace_sources(source, files.paths())?;
            let key = self
                .workspace_edits
                .preparing_origin()
                .ok_or("workspace source origin missing")?;
            let drafts = self.language.decode_edit(key, files)?;
            self.workspace_edits.finish_preparation(
                drafts,
                self.workspace.edit_scope(),
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        match result {
            Ok(()) => self.show_workspace_proposal(),
            Err(error) => self.abort_workspace_edit(error),
        }
    }
}

impl App {
    pub(in crate::app) fn reject_workspace_picker(&mut self, id: crate::workspace::ProposalId) {
        if self.workspace_edits.proposal().is_none_or(|p| p.id != id) {
            return;
        }
        let invalidated = self.workspace_edits.reject();
        for request in invalidated.requests {
            self.documents.cancel_request(request);
        }
        if let Some(key) = invalidated.origin {
            let _ = self.finish_workspace_edit_origin(
                &key,
                false,
                Some("workspace preview dismissed".into()),
            );
        }
    }
}

impl App {
    fn finish_workspace_edit_origin(
        &mut self,
        key: &crate::app::workspace::edits::OriginKey,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        if self.extension.owns_edit(key) {
            self.extension.finish_edit(key)
        } else {
            self.language.finish_edit(key, applied, reason, &self.state)
        }
    }
}

impl App {
    pub(in crate::app) fn abort_workspace_edit(&mut self, reason: String) {
        let invalidated = self.workspace_edits.reject();
        for id in invalidated.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = invalidated.origin {
            let _ = self.finish_workspace_edit_origin(&origin, false, Some(reason.clone()));
        }
        self.close_picker();
        self.show_error(reason);
    }
}

impl App {
    pub(in crate::app) fn reject_workspace_proposal(&mut self) {
        let invalidated = self.workspace_edits.reject();
        for id in invalidated.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = invalidated.origin {
            let _ = self.finish_workspace_edit_origin(
                &origin,
                false,
                Some("workspace edit rejected".into()),
            );
        }
        self.close_picker();
        self.notify_message(self.message("workspace_edit_rejected", &[]), false);
    }
}

impl App {
    pub(in crate::app) fn accept_workspace_proposal(&mut self) {
        if let Err(error) = self.check_workspace_edit_origin() {
            self.abort_workspace_edit(error);
            return;
        }
        let transaction = self.transaction_id();
        let prepared = (|| {
            let (source, _) = self.workspace_edits.approval(
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )?;
            let changes = self
                .workspace_edits
                .resources()
                .iter()
                .map(|intent| {
                    let target = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => Some(target.as_path()),
                        ResourceIntentKind::Delete => None,
                    };
                    self.state.check_resource_destination(target)?;
                    self.state.workspace_resource_change(source, &intent.source)
                })
                .collect::<Result<Vec<_>, String>>()?;
            self.workspace_edits.prepare_approval(
                &self.state,
                transaction,
                changes,
                &self.workspace.version(),
                self.configuration.binding_revision(),
            )
        })();
        let (draft, changes) = match prepared {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.finish_workspace_approval(draft, changes);
    }
}

impl App {
    pub(in crate::app) fn process_workspace_approval(&mut self) {
        let Some(result) = self.workspace_edits.poll_approval(
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        if let Err(error) = self.check_workspace_edit_origin() {
            self.abort_workspace_edit(error);
            return;
        }
        match result {
            Ok((draft, changes)) => self.finish_workspace_approval(draft, changes),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    fn finish_workspace_approval(
        &mut self,
        draft: crate::editor::EditorDraft,
        changes: Vec<crate::editor::BufferPathChange>,
    ) {
        if let Err(error) = self.workspace_edits.stage_approval(&draft, changes) {
            self.show_error(error);
            return;
        }
        if let Err(error) = self.commit_editor_draft(draft, None, false) {
            self.workspace_edits.abandon_approval();
            self.show_error(error);
            return;
        }
        let proposal = self
            .workspace_edits
            .proposal()
            .expect("approved preview")
            .id;
        let accepted = self.workspace_edits.accepted(self.state.publication());
        if accepted.is_ok() {
            self.close_picker();
        }
        match accepted {
            Ok(Some(origin)) => {
                let result = if !self.extension.owns_edit(&origin)
                    && self.workspace_edits.resources_pending(proposal)
                {
                    self.language.defer_edit(&origin, proposal)
                } else {
                    self.finish_workspace_edit_origin(&origin, true, None)
                };
                if let Err(error) = result {
                    self.show_error(error);
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.show_error(error);
                return;
            }
        }
        self.notify_message(self.message("workspace_edit_applied", &[]), false);
    }
}

impl App {
    /// Applies an edit whose positions use `encoding`, the encoding of the server that sent it.
    pub(in crate::app) fn apply_lsp_workspace_edit(
        &mut self,
        edit: LspWorkspaceEdit,
        encoding: PositionEncoding,
    ) -> Result<(), String> {
        let id = self.transaction_id();
        let documents = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                Some(crate::app::language::edits::EditDocument {
                    id,
                    path: buffer.path.as_deref()?,
                    text: &buffer.text,
                    dirty: buffer.dirty,
                })
            })
            .collect::<Vec<_>>();
        let transaction = crate::app::language::edits::LspEditSources::completion_transaction(
            edit, encoding, &documents, id,
        )?;
        let draft = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())?;
        self.commit_editor_draft(draft, None, false)?;
        Ok(())
    }
}

impl App {
    pub(in crate::app) fn show_workspace_proposal(&mut self) {
        let Some(proposal) = self.workspace_edits.proposal() else {
            return;
        };
        let mut items = proposal
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let id = PickerItemId(format!(
                    "proposal-file:{index}:{}",
                    file.canonical_path.display()
                ));
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id,
                        label: self.workspace_relative_label(&file.canonical_path),
                        detail: self.message(
                            if file.selected {
                                "workspace_edit_included"
                            } else {
                                "workspace_edit_excluded"
                            },
                            &[("count", file.edit_count.to_string())],
                        ),
                        source: String::new(),
                        action: PickerAction::None,
                    },
                    PickerPayload::ProposalFile(index),
                )
            })
            .collect::<Vec<_>>();
        items.extend(
            self.workspace_edits
                .resources()
                .iter()
                .enumerate()
                .map(|(index, intent)| {
                    let detail = match &intent.kind {
                        ResourceIntentKind::Rename { target, .. } => self.message(
                            "workspace_resource_rename_row",
                            &[("target", target.display().to_string())],
                        ),
                        ResourceIntentKind::Delete => {
                            self.message("workspace_resource_delete_row", &[])
                        }
                    };
                    (
                        PickerItem {
                            badge: String::new(),
                            kind: crate::workspace::PickerItemKind::Entry,
                            id: PickerItemId(format!("resource-intent:{index}")),
                            label: self.workspace_relative_label(&intent.source),
                            detail,
                            source: self.message("workspace_resource_staged", &[]),
                            action: PickerAction::None,
                        },
                        PickerPayload::Command(String::new()),
                    )
                }),
        );
        let title = proposal.label.clone();
        let id = proposal.id;
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            title,
            items,
            String::new(),
            true,
            PickerOrigin::WorkspaceEdit(id),
        );
    }
}
