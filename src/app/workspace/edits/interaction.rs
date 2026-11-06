use crate::app::search::SEARCH_MAXIMUM_FILE_BYTES;
use crate::app::App;
use crate::app::LspWorkspaceEdit;
use crate::app::PickerAction;
use crate::app::PickerItem;
use crate::app::PickerItemId;
use crate::app::PickerOrigin;
use crate::app::PickerPayload;
use crate::app::PickerSurfaceKind;
use crate::app::PositionEncoding;
use crate::app::ResourceIntentKind;
use crate::app::VfsResult;







impl App {

    pub(in crate::app) fn process_workspace_io(&mut self, reply: crate::app::documents::WorkspaceIoReply) {
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

    pub(in crate::app) fn retire_workspace_resources(&mut self, reason: &str) {
        self.language.cancel_deferred_edits(None);
        let (notices, requests) = self.workspace_edits.retire_resources(reason);
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
                    "resource publication retired; its filesystem outcome is still pending",
                );
            }
        }
    }
}

impl App {

    pub(in crate::app) fn finish_workspace_resource_obligations(
        &mut self,
        reason: &str,
    ) {
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
            let snapshot =
                if let crate::app::workspace::edits::InputFile::Read(path) = file {
                    self.state.workspace_snapshot(
                        self.workspace_edits
                            .preparing_sources()
                            .expect("admitted source"),
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
                let admitted = self
                    .workspace_edits
                    .prepare_io(
                        index,
                        &self.workspace,
                        self.configuration.binding_revision_ref(),
                        &self.state,
                    )
                    .and_then(|permit| self.documents.submit_workspace_io(permit));
                match admitted {
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

    pub(in crate::app) fn retire_workspace_picker(
        &mut self,
        id: crate::workspace::ProposalId,
    ) {
        if self.workspace_edits.proposal().is_none_or(|p| p.id != id) {
            return;
        }
        let retired = self.workspace_edits.reject();
        for request in retired.requests {
            self.documents.cancel_request(request);
        }
        if let Some(key) = retired.origin {
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
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
            let _ = self.finish_workspace_edit_origin(&origin, false, Some(reason.clone()));
        }
        self.close_picker();
        self.show_error(reason);
    }
}

impl App {

    pub(in crate::app) fn reject_workspace_proposal(&mut self) {
        let retired = self.workspace_edits.reject();
        for id in retired.requests {
            self.documents.cancel_request(id);
        }
        if let Some(origin) = retired.origin {
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
        let transaction =
            crate::app::language::edits::LspEditSources::completion_transaction(
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
