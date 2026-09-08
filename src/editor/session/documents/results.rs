use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_vfs_results(&mut self) {
        loop {
            let result = match self
                .documents
                .poll(&self.state.window_version(), self.workspace.root_revision())
            {
                Ok(Some(DocumentEvent::Legacy(result))) => result,
                Ok(Some(DocumentEvent::TaskRead(reply))) => {
                    if let Err(error) = self.tasks.accept_linter_read(
                        reply,
                        &self.state,
                        &self.workspace,
                        self.configuration.binding_revision(),
                        &self.diagnostics,
                    ) {
                        self.show_error(error);
                    }
                    self.drain_linter_sources();
                    continue;
                }
                Ok(Some(DocumentEvent::WorkspaceIo(reply))) => {
                    self.process_workspace_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Explorer(reply))) => {
                    self.process_explorer_io(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::File(reply))) => {
                    self.process_file_reply(reply);
                    continue;
                }
                Ok(Some(DocumentEvent::Stopped {
                    requests,
                    error,
                    task_reads,
                })) => {
                    for id in requests {
                        self.pending_vfs.remove(&id);
                    }
                    self.search.clear_preview();
                    self.explorer.clear_preview();
                    self.explorer.retire_intents();
                    self.session_restore = None;
                    if self.workspace_edits.proposal().is_some() || self.workspace_edits.preparing()
                    {
                        self.reject_workspace_proposal();
                    }
                    self.finish_workspace_resource_obligations("document worker stopped");
                    self.tasks.linter_worker_stopped(task_reads);
                    self.drain_linter_sources();
                    self.show_error(error);
                    break;
                }
                Ok(Some(DocumentEvent::Discarded)) => continue,
                Ok(Some(DocumentEvent::Startup {
                    receipt,
                    path,
                    text,
                })) => {
                    let buffer = match text
                        .and_then(|text| self.open_startup_document(text, path.clone()))
                    {
                        Ok(buffer) => {
                            self.notify_message(
                                self.message("opened", &[("path", path.display().to_string())]),
                                false,
                            );
                            Some(buffer)
                        }
                        Err(error) => {
                            self.show_error(error);
                            None
                        }
                    };
                    self.documents.finish_startup(
                        receipt,
                        buffer,
                        &self.state.window_version(),
                        self.workspace.root_revision(),
                    );
                    self.finish_startup_reads_if_complete();
                    continue;
                }
                Ok(None) => break,
                Err(error) => {
                    self.show_error(error);
                    break;
                }
            };
            match result {
                VfsResult::RecoveryDiscovered { id, discovery } => {
                    if matches!(
                        self.pending_vfs.remove(&id),
                        Some(PendingVfs::RecoveryDiscovery)
                    ) {
                        self.recovery_records = discovery.records;
                        if !discovery.diagnostics.is_empty() {
                            self.show_error(discovery.diagnostics.join("; "));
                        }
                    }
                }
                VfsResult::RecoveryCleaned { id, cleanup } => {
                    if matches!(
                        self.pending_vfs.remove(&id),
                        Some(PendingVfs::RecoveryCleanup)
                    ) {
                        self.recovery_records
                            .retain(|(path, _)| !cleanup.deleted.contains(path));
                        let pruned_live = self
                            .recovery_revisions
                            .iter()
                            .filter_map(|(buffer, revision)| {
                                let path = self.recovery_path(*buffer);
                                let newer_write_pending =
                                    self.pending_vfs.values().any(|pending| {
                                        matches!(
                                            pending,
                                            PendingVfs::RecoveryWrite {
                                                buffer: pending_buffer,
                                                ..
                                            } if pending_buffer == buffer
                                        )
                                    });
                                (cleanup.deleted.contains(&path) && !newer_write_pending)
                                    .then_some((*buffer, *revision))
                            })
                            .collect::<Vec<_>>();
                        for (buffer, revision) in pruned_live {
                            self.recovery_revisions.remove(&buffer);
                            self.recovery_pruned_revisions.insert(buffer, revision);
                        }
                        if !cleanup.diagnostics.is_empty() {
                            self.show_error(cleanup.diagnostics.join("; "));
                        } else if !cleanup.deleted.is_empty() {
                            self.notify_message(
                                self.message(
                                    "recovery_pruned",
                                    &[("count", cleanup.deleted.len().to_string())],
                                ),
                                false,
                            );
                        }
                    }
                }
                VfsResult::RecoveryWritten {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::RecoveryWrite { .. }), Ok(())) => {
                            self.recovery_cleanup_queued = false;
                        }
                        (Some(PendingVfs::RecoveryWrite { buffer, revision }), Err(error)) => {
                            if self.recovery_revisions.get(&buffer) == Some(&revision) {
                                self.recovery_revisions.remove(&buffer);
                            }
                            self.show_error(error);
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Deleted {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::CrashDelete), Ok(())) => {
                            self.notify_message(self.message("crash_cleared", &[]), false);
                        }
                        (
                            Some(PendingVfs::RecoveryDelete {
                                path: expected,
                                buffer,
                            }),
                            Ok(()),
                        ) => {
                            self.recovery_records
                                .retain(|(record_path, _)| *record_path != expected);
                            if let Some((buffer, expected_revision)) = buffer {
                                if self.recovery_revisions.get(&buffer) == Some(&expected_revision)
                                {
                                    self.recovery_revisions.remove(&buffer);
                                }
                                if self.recovery_pruned_revisions.get(&buffer)
                                    == Some(&expected_revision)
                                {
                                    self.recovery_pruned_revisions.remove(&buffer);
                                }
                            } else {
                                self.notify_message(self.message("recovery_discarded", &[]), false);
                            }
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::ExplorerMutated { .. } => {
                    self.show_error("unowned explorer mutation result")
                }
                VfsResult::ResourceApplied { .. }
                | VfsResult::ResourceResolved { .. }
                | VfsResult::CreateTarget { .. } => self.show_error("unowned workspace I/O result"),
                VfsResult::Read { id, path, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    if self.search.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_picker_ui();
                        continue;
                    }
                    if self.explorer.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_explorer_ui();
                        continue;
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(PendingVfs::SessionRestore { requested_path }) => {
                                self.accept_session_restore_read(requested_path, path, text)
                            }

                            Some(PendingVfs::MatcherNavigation { source }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let start = self.state.buffers[buffer]
                                    .text
                                    .line_to_byte(source.line)
                                    .ok()
                                    .map(|offset| {
                                        ByteOffset(
                                            (offset.get() + source.column)
                                                .min(self.state.buffers[buffer].text.len_bytes()),
                                        )
                                    })
                                    .filter(|offset| {
                                        self.state.buffers[buffer]
                                            .text
                                            .validate_offset(*offset)
                                            .is_ok()
                                    })
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::PersistedNavigation { location }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let cursor = ByteOffset(location.byte.min(length));
                                let anchor = ByteOffset(location.anchor.min(length));
                                self.state.views[view].cursor = cursor;
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].scroll_wrap_row = 0;
                                self.state.views[view].scroll_column = location.scroll_column;
                                self.state.views[view].selection =
                                    (anchor != cursor).then_some(crate::editor::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editor::SelectionKind::Character,
                                    });
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::LspNavigation { range, encoding }) => {
                                let (buffer, view) = self.state.open_text(text, Some(path.clone()));
                                let position = self.state.buffers.get(buffer).and_then(|buffer| {
                                    PositionCodec::new(&buffer.text)
                                        .lsp_to_byte(
                                            range.start.line,
                                            range.start.character,
                                            encoding,
                                        )
                                        .ok()
                                });
                                if let Some(position) = position {
                                    let transaction = EditorTransaction::new(self.transaction_id())
                                        .with_precondition(Precondition::ViewDisplays {
                                            view,
                                            buffer,
                                        })
                                        .with_operation(EditorOperation::SetCursor {
                                            view,
                                            position,
                                        })
                                        .with_undo_policy(UndoPolicy::Ignore);
                                    if let Err(error) = self.state.apply_transaction(&transaction) {
                                        self.show_error(error);
                                    }
                                } else {
                                    self.show_error(self.message("lsp_invalid_position", &[]));
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::WorkspaceSearchNavigation { range }) => {
                                let start = ByteOffset(range.start);
                                let end = ByteOffset(range.end);
                                if text.validate_offset(start).is_err()
                                    || text.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = self.state.open_text(text, Some(path));
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_precondition(Precondition::ViewDisplays { view, buffer })
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: start,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                }
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::Completion { key }), Ok(paths)) => {
                            self.input.accept_prompt_paths(
                                &key,
                                paths,
                                &self.state.workspace_root,
                                self.configuration.settings(),
                            );
                            self.project_input_prompt();
                        }
                        (Some(PendingVfs::Completion { .. }), Err(_)) => {}
                        (Some(PendingVfs::SessionRestore { .. }), Err(_)) => {
                            self.fail_session_restore()
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { result, .. } => {
                    if let Err(error) = result {
                        self.show_error(error);
                    } else {
                        self.show_error(self.message("unexpected_completion", &[]));
                    }
                }
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_explorer_io(
        &mut self,
        reply: crate::files::ExplorerIoReply,
    ) {
        use crate::editor::session::explorer::MutationUpdate;
        if let crate::files::ExplorerIoReply::Opened { id, path, result } = reply {
            let Some(reply) = self.explorer.accept_open(
                id,
                &path,
                (
                    self.workspace.version(),
                    self.configuration.binding_revision(),
                ),
                &self.state.window_version(),
            ) else {
                return;
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            if let Err(error) = self
                .window_session
                .check_editor_admission(&(&self.state).into())
            {
                self.show_error(error);
                return;
            }
            let text = match result
                .map_err(|e| e.to_string())
                .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|e| e.to_string()))
            {
                Ok(text) => text,
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            };
            let activate = self.window_session.navigation_current(
                &reply.navigation,
                &self.state.window_version(),
                self.state.active_view,
            );
            let mut draft = self.state.draft();
            if activate {
                if reply.split
                    && self.state.active_view.is_some()
                    && let Err(error) =
                        draft.split_document(self.configuration.settings().ui.panes.default_ratio)
                {
                    self.show_error(error);
                    return;
                }
                draft.open_document(text, path);
            } else {
                draft.load_document(text, path);
            }
            if let Err(error) = self.commit_editor_draft(draft, None, activate) {
                self.show_error(error);
                return;
            }
            if activate {
                self.set_dashboard_visibility(false);
                self.ensure_cursor_visible();
            }
            return;
        }
        let Some(update) = self.explorer.accept_mutation(
            reply,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) else {
            return;
        };
        match update {
            MutationUpdate::Confirmation(path) => {
                self.open_prompt(PromptKind::ExplorerDelete, String::new());
                self.prompt_help(format!(
                    "Permanently deletes `{}`. Type `delete`.",
                    path.display()
                ));
            }
            MutationUpdate::Apply(prepared) => self.apply_prepared_explorer(prepared),
            MutationUpdate::Failed(error) => self.show_error(error),
            MutationUpdate::Complete { operation, change } => {
                let reconciliation = (|| -> Result<(), String> {
                    self.window_session
                        .check_editor_admission(&(&self.state).into())?;
                    if let Some(change) = change {
                        let target = match &operation {
                            ExplorerMutation::Rename { target, .. } => Some(target.as_path()),
                            _ => None,
                        };
                        let (draft, recent) = self.state.reconcile_paths(change, target)?;
                        if let Some(draft) = draft {
                            self.commit_editor_draft(draft, None, false)?;
                        }
                        for path in recent {
                            self.record_recent_file(path);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = reconciliation {
                    self.show_error(error);
                } else {
                    self.notify_message(
                        format!(
                            "explorer operation complete: {}",
                            operation
                                .target()
                                .or_else(|| operation.source())
                                .map_or_else(|| "path".into(), |path| path.display().to_string())
                        ),
                        false,
                    );
                }
                self.refresh_explorer();
                self.start_git_status_refresh_silent();
            }
        }
    }
}

impl InteractiveApplication {
    fn process_workspace_io(&mut self, reply: crate::editor::session::documents::WorkspaceIoReply) {
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
                    self.show_error(error);
                    return;
                }
                for path in recent {
                    self.record_recent_file(path);
                }
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn retire_workspace_resources(&mut self, reason: &str) {
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
    pub(in crate::editor::session) fn finish_workspace_resource_obligations(
        &mut self,
        reason: &str,
    ) {
        for notice in self.workspace_edits.finish_resources(reason) {
            self.show_error(notice);
        }
    }
}
