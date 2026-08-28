use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn begin_workspace_proposal(
        &mut self,
        label: String,
        edit: LspWorkspaceEdit,
        response: Option<(String, crate::language::lsp::ServerRequestId)>,
        encoding: PositionEncoding,
    ) {
        let mut create_paths = Vec::new();
        let mut resource_paths = Vec::<(PathBuf, Option<PathBuf>)>::new();
        for resource in &edit.resources {
            match resource {
                crate::language::lsp::LspResourceOperation::Create {
                    uri,
                    overwrite,
                    ignore_if_exists,
                } if !overwrite && !ignore_if_exists => {
                    let Some(path) = path_from_file_uri(uri) else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    create_paths.push(path)
                }
                crate::language::lsp::LspResourceOperation::Rename {
                    old_uri,
                    new_uri,
                    overwrite,
                    ignore_if_exists,
                } if !overwrite && !ignore_if_exists => {
                    let (Some(source), Some(target)) =
                        (path_from_file_uri(old_uri), path_from_file_uri(new_uri))
                    else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    resource_paths.push((source, Some(target)));
                }
                crate::language::lsp::LspResourceOperation::Delete {
                    uri,
                    recursive,
                    ignore_if_not_exists,
                } if !recursive && !ignore_if_not_exists => {
                    let Some(source) = path_from_file_uri(uri) else {
                        self.show_error(self.message("workspace_resource_unsupported", &[]));
                        return;
                    };
                    resource_paths.push((source, None));
                }
                _ => {
                    if let Some((server, request_id)) = &response {
                        let _ = self.lsp.respond_apply_edit(
                            server,
                            request_id.clone(),
                            false,
                            Some(self.message("workspace_resource_unsupported", &[])),
                        );
                    }
                    self.show_error(self.message("workspace_resource_unsupported", &[]));
                    return;
                }
            }
        }
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            if let Some((server, request_id)) = response {
                let _ = self.lsp.respond_apply_edit(
                    &server,
                    request_id,
                    false,
                    Some("another workspace edit is already being previewed".into()),
                );
            }
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.saturating_add(1);
        self.workspace_proposal_root_revision = Some(self.workspace.root_revision());
        let mut pending = PendingWorkspaceProposal {
            id,
            encoding,
            label,
            documents: edit.documents,
            snapshots: Vec::new(),
            creates: Vec::new(),
            resource_intents: Vec::new(),
            resolved_paths: HashMap::new(),
            waiting: 0,
            response,
        };
        for document in pending.documents.clone() {
            let Some(path) = path_from_file_uri(&document.uri) else {
                self.reject_pending_workspace_proposal(
                    pending,
                    format!("unsupported workspace-edit URI `{}`", document.uri),
                );
                return;
            };
            if self.workspace.require_path(&path).is_err() {
                self.reject_pending_workspace_proposal(
                    pending,
                    format!(
                        "workspace edit target is outside workspace roots: {}",
                        path.display()
                    ),
                );
                return;
            }
            if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                pending.resolved_paths.insert(path.clone(), path.clone());
                pending.snapshots.push(FileSnapshot {
                    canonical_path: path,
                    revision: Some(buffer.text.revision()),
                    bytes: buffer.text.to_bytes(),
                });
            } else {
                let roots = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect();
                match self.vfs.read_scoped(path.clone(), roots) {
                    Ok(request) => {
                        self.pending_vfs.insert(
                            request,
                            PendingVfs::WorkspaceProposalRead {
                                proposal: id,
                                requested_path: path,
                            },
                        );
                        pending.waiting = pending.waiting.saturating_add(1);
                    }
                    Err(error) => {
                        self.reject_pending_workspace_proposal(pending, error.to_string());
                        return;
                    }
                }
            }
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        for path in create_paths {
            match self.vfs.resolve_create(path.clone(), roots.clone()) {
                Ok(request) => {
                    self.pending_vfs.insert(
                        request,
                        PendingVfs::WorkspaceProposalCreate {
                            proposal: id,
                            requested_path: path,
                        },
                    );
                    pending.waiting = pending.waiting.saturating_add(1)
                }
                Err(error) => {
                    self.reject_pending_workspace_proposal(pending, error.to_string());
                    return;
                }
            }
        }
        for (source, target) in resource_paths {
            match self.vfs.resolve_resource(
                source,
                target,
                roots.clone(),
                self.configuration
                    .settings
                    .workspace
                    .search
                    .maximum_file_bytes as usize,
            ) {
                Ok(request) => {
                    self.pending_vfs.insert(
                        request,
                        PendingVfs::WorkspaceResourceResolve { proposal: id },
                    );
                    pending.waiting = pending.waiting.saturating_add(1)
                }
                Err(error) => {
                    self.reject_pending_workspace_proposal(pending, error.to_string());
                    return;
                }
            }
        }
        self.pending_workspace_proposal = Some(pending);
        if self
            .pending_workspace_proposal
            .as_ref()
            .is_some_and(|pending| pending.waiting == 0)
        {
            self.finish_workspace_proposal();
        } else {
            self.ui.notification = Some(self.message("workspace_edit_loading", &[]));
            self.ui.notification_error = false;
        }
    }

    pub(in crate::editor::session) fn reject_pending_workspace_proposal(
        &mut self,
        pending: PendingWorkspaceProposal,
        reason: String,
    ) {
        if let Some((server, request_id)) = pending.response {
            let _ = self
                .lsp
                .respond_apply_edit(&server, request_id, false, Some(reason.clone()));
        }
        self.show_error(self.message("workspace_edit_failed", &[("diagnostic", reason)]));
    }

    pub(in crate::editor::session) fn finish_workspace_proposal(&mut self) {
        let Some(pending) = self.pending_workspace_proposal.take() else {
            return;
        };
        let encoding = pending.encoding;
        let mut drafts = pending
            .documents
            .iter()
            .filter_map(|document| {
                let requested = path_from_file_uri(&document.uri)?;
                let canonical = pending.resolved_paths.get(&requested)?.clone();
                let snapshot = pending
                    .snapshots
                    .iter()
                    .find(|snapshot| snapshot.canonical_path == canonical)?;
                let text = TextBuffer::from_bytes(&snapshot.bytes).ok()?;
                let codec = PositionCodec::new(&text);
                let edits = document
                    .edits
                    .iter()
                    .map(|edit| {
                        let start = codec
                            .lsp_to_byte(
                                edit.range.start.line,
                                edit.range.start.character,
                                encoding,
                            )
                            .map_err(|error| error.to_string())?;
                        let end = codec
                            .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                            .map_err(|error| error.to_string())?;
                        Ok(TextEdit::new(
                            ByteRange::new(start, end).map_err(|error| error.to_string())?,
                            edit.new_text.clone(),
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
                    .ok()?;
                Some(WorkspaceEditDraft {
                    canonical_path: canonical,
                    expected_revision: document.version.map(BufferRevision).or(snapshot.revision),
                    edits,
                })
            })
            .collect::<Vec<_>>();
        if drafts.len() != pending.documents.len() {
            self.reject_pending_workspace_proposal(
                pending,
                "workspace edit contains invalid positions or missing snapshots".into(),
            );
            return;
        }
        for path in &pending.creates {
            drafts.push(WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: None,
                edits: Vec::new(),
            });
        }
        match WorkspaceEditProposal::build(
            pending.id,
            pending.label.clone(),
            &self.workspace,
            drafts,
            pending.snapshots.clone(),
        ) {
            Ok(proposal) => {
                self.workspace_resource_intents = pending.resource_intents.clone();
                self.workspace_proposal_response = pending.response.clone();
                self.workspace_proposal = Some(proposal);
                self.show_workspace_proposal();
            }
            Err(error) => self.reject_pending_workspace_proposal(pending, error.to_string()),
        }
    }

    pub(in crate::editor::session) fn show_workspace_proposal(&mut self) {
        let Some(proposal) = &self.workspace_proposal else {
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
                        id,
                        label: file.canonical_path.display().to_string(),
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
            self.workspace_resource_intents
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
                            id: PickerItemId(format!("resource-intent:{index}")),
                            label: intent.source.display().to_string(),
                            detail,
                            source: self.message("workspace_resource_staged", &[]),
                            action: PickerAction::None,
                        },
                        PickerPayload::Command(String::new()),
                    )
                }),
        );
        let title = proposal.label.clone();
        self.install_picker(title, items);
    }

    pub(in crate::editor::session) fn reject_workspace_proposal(&mut self) {
        if let Some(mut proposal) = self.workspace_proposal.take() {
            let _ = proposal.reject();
        }
        if let Some((server, request_id)) = self.workspace_proposal_response.take() {
            let _ = self.lsp.respond_apply_edit(
                &server,
                request_id,
                false,
                Some("workspace edit rejected by user".into()),
            );
        }
        self.workspace_proposal_root_revision = None;
        self.workspace_resource_intents.clear();
        self.close_picker();
        self.ui.notification = Some(self.message("workspace_edit_rejected", &[]));
        self.ui.notification_error = false;
    }

    pub(in crate::editor::session) fn accept_workspace_proposal(&mut self) {
        if self.workspace_proposal_root_revision != Some(self.workspace.root_revision()) {
            self.reject_workspace_proposal();
            self.show_error(self.message("workspace_root_stale", &[]));
            return;
        }
        let Some(proposal) = &self.workspace_proposal else {
            return;
        };
        let selected = proposal
            .files
            .iter()
            .filter(|file| file.selected)
            .cloned()
            .collect::<Vec<_>>();
        let mut targets = Vec::new();
        let mut created_buffers = Vec::new();
        for file in &selected {
            let buffer = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&file.canonical_path))
                .map(|(buffer, _)| buffer)
                .unwrap_or_else(|| {
                    let text = TextBuffer::from_bytes(&file.original_bytes)
                        .expect("validated workspace proposal snapshot remains valid");
                    self.state
                        .load_hidden_text(text, file.canonical_path.clone())
                });
            if file.base_revision.is_none() {
                created_buffers.push(buffer);
            }
            let current = self.state.buffers[buffer].text.revision();
            if file
                .base_revision
                .is_some_and(|expected| expected != current)
            {
                self.show_error(self.message(
                    "workspace_edit_failed",
                    &[(
                        "diagnostic",
                        format!("{} changed after preview", file.canonical_path.display()),
                    )],
                ));
                return;
            }
            let replacement = match TextBuffer::from_bytes(&file.proposed_bytes) {
                Ok(text) => text.to_string(),
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            targets.push((buffer, current, replacement));
        }
        let mut transaction = EditorTransaction::new(self.transaction_id());
        transaction.label = self
            .workspace_proposal
            .as_ref()
            .map(|proposal| proposal.label.clone());
        for (buffer, revision, replacement) in targets {
            transaction
                .preconditions
                .push(Precondition::BufferRevision { buffer, revision });
            transaction
                .preconditions
                .push(Precondition::BufferWritable(buffer));
            let end = ByteOffset(self.state.buffers[buffer].text.len_bytes());
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    revision,
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), end).expect("ordered range"),
                        replacement,
                    )],
                ),
            });
        }
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        for buffer in created_buffers {
            self.state.set_buffer_dirty(buffer, true);
        }
        if let Some(proposal) = &mut self.workspace_proposal {
            let _ = proposal.accept();
        }
        self.workspace_proposal = None;
        self.workspace_proposal_root_revision = None;
        self.accepted_resource_intents
            .extend(std::mem::take(&mut self.workspace_resource_intents));
        if let Some((server, request_id)) = self.workspace_proposal_response.take() {
            let _ = self.lsp.respond_apply_edit(&server, request_id, true, None);
        }
        self.close_picker();
        self.ui.notification = Some(self.message("workspace_edit_applied", &[]));
        self.ui.notification_error = false;
    }

    pub(in crate::editor::session) fn apply_lsp_workspace_edit(
        &mut self,
        edit: LspWorkspaceEdit,
    ) -> Result<(), String> {
        let encoding = self
            .lsp_capabilities
            .as_ref()
            .map_or(PositionEncoding::Utf16, |capabilities| {
                capabilities.position_encoding
            });
        let mut operations = Vec::new();
        let mut preconditions = Vec::new();
        for document in edit.documents {
            let path = path_from_file_uri(&document.uri)
                .ok_or_else(|| format!("unsupported workspace-edit URI `{}`", document.uri))?;
            let (buffer_id, buffer) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                .ok_or_else(|| {
                    format!(
                        "workspace edit targets unopened file `{}`; preview required",
                        path.display()
                    )
                })?;
            let revision = buffer.text.revision();
            if document
                .version
                .is_some_and(|version| version != revision.0)
            {
                return Err(format!("workspace edit for `{}` is stale", path.display()));
            }
            let codec = PositionCodec::new(&buffer.text);
            let mut edits = Vec::with_capacity(document.edits.len());
            for edit in document.edits {
                let start = codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .map_err(|error| error.to_string())?;
                let end = codec
                    .lsp_to_byte(edit.range.end.line, edit.range.end.character, encoding)
                    .map_err(|error| error.to_string())?;
                let range = ByteRange::new(start, end).map_err(|error| error.to_string())?;
                edits.push(TextEdit::new(range, edit.new_text));
            }
            buffer
                .text
                .validate_edits(&TextEditSet::new(revision, edits.clone()))
                .map_err(|error| error.to_string())?;
            preconditions.push(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            });
            preconditions.push(Precondition::BufferWritable(buffer_id));
            operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, edits),
            });
        }
        let mut transaction = EditorTransaction::new(self.transaction_id());
        transaction.preconditions = preconditions;
        transaction.operations = operations;
        transaction.label = Some("LSP workspace edit".into());
        self.state
            .apply_transaction(&transaction)
            .map_err(|error| error.to_string())?;
        self.ui.notification = Some(self.message("lsp_edit_applied", &[]));
        self.ui.notification_error = false;
        Ok(())
    }
}
