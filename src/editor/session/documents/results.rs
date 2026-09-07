use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_vfs_results(&mut self) {
        loop {
            let result = match self.vfs.try_result() {
                Ok(Some(result)) => result,
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
                            self.ui.notification = Some(self.message(
                                "recovery_pruned",
                                &[("count", cleanup.deleted.len().to_string())],
                            ));
                            self.ui.notification_error = false;
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
                            self.ui.notification = Some(self.message("crash_cleared", &[]));
                            self.ui.notification_error = false;
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
                                self.ui.notification =
                                    Some(self.message("recovery_discarded", &[]));
                                self.ui.notification_error = false;
                            }
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::ResourceResolved { id, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (Some(PendingVfs::WorkspaceResourceResolve { proposal }), Ok(intent)) => {
                            let mut finish = false;
                            if let Some(build) = &mut self.pending_workspace_proposal
                                && build.id == proposal
                            {
                                if let ResourceIntentKind::Rename { target, .. } = &intent.kind
                                    && self
                                        .state
                                        .buffers
                                        .iter()
                                        .any(|(_, buffer)| buffer.path.as_ref() == Some(target))
                                {
                                    if let Some(build) = self.pending_workspace_proposal.take() {
                                        self.reject_pending_workspace_proposal(
                                            build,
                                            "rename target already has an editor buffer".into(),
                                        )
                                    }
                                    continue;
                                }
                                build.resource_intents.push(intent);
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            if finish {
                                self.finish_workspace_proposal();
                            }
                        }
                        (Some(PendingVfs::WorkspaceResourceResolve { .. }), Err(error)) => {
                            if let Some(build) = self.pending_workspace_proposal.take() {
                                self.reject_pending_workspace_proposal(build, error.to_string())
                            }
                        }
                        _ => {}
                    }
                }
                VfsResult::ResourceApplied { id, intent, result } => {
                    self.resource_apply_pending.remove(&id);
                    match result {
                        Ok(()) => match intent.kind {
                            ResourceIntentKind::Rename { target, .. } => {
                                if let Some((buffer, _)) =
                                    self.state.buffers.iter().find(|(_, buffer)| {
                                        buffer.path.as_ref() == Some(&intent.source)
                                    })
                                {
                                    let dirty = self.state.buffers[buffer].dirty;
                                    self.state.set_buffer_path_and_dirty(
                                        buffer,
                                        Some(target.clone()),
                                        dirty,
                                    );
                                    self.record_recent_file(target);
                                }
                            }
                            ResourceIntentKind::Delete => {
                                if let Some((buffer, _)) =
                                    self.state.buffers.iter().find(|(_, buffer)| {
                                        buffer.path.as_ref() == Some(&intent.source)
                                    })
                                {
                                    self.state.set_buffer_path_and_dirty(buffer, None, true);
                                }
                            }
                        },
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::ExplorerMutated {
                    id,
                    operation,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    if !matches!(pending, Some(PendingVfs::ExplorerMutation)) {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            match &operation {
                                ExplorerMutation::Rename { source, target } => {
                                    let affected = self
                                        .state
                                        .buffers
                                        .iter()
                                        .filter_map(|(id, buffer)| {
                                            let path = buffer.path.as_ref()?;
                                            let suffix = path.strip_prefix(source).ok()?;
                                            Some((id, target.join(suffix), buffer.dirty))
                                        })
                                        .collect::<Vec<_>>();
                                    for (buffer, path, dirty) in affected {
                                        self.state.set_buffer_path_and_dirty(
                                            buffer,
                                            Some(path.clone()),
                                            dirty,
                                        );
                                        self.record_recent_file(path);
                                    }
                                    if self
                                        .explorer_clipboard
                                        .as_ref()
                                        .is_some_and(|(path, cut)| *cut && path == source)
                                    {
                                        self.explorer_clipboard = None;
                                    }
                                }
                                ExplorerMutation::Delete { path } => {
                                    let affected = self
                                        .state
                                        .buffers
                                        .iter()
                                        .filter_map(|(id, buffer)| {
                                            buffer
                                                .path
                                                .as_ref()
                                                .is_some_and(|candidate| {
                                                    candidate.starts_with(path)
                                                })
                                                .then_some(id)
                                        })
                                        .collect::<Vec<_>>();
                                    for buffer in affected {
                                        self.state.set_buffer_path_and_dirty(buffer, None, true);
                                    }
                                }
                                ExplorerMutation::Create { .. } | ExplorerMutation::Copy { .. } => {
                                }
                            }
                            self.ui.notification = Some(format!(
                                "explorer operation complete: {}",
                                operation
                                    .target()
                                    .or_else(|| operation.source())
                                    .map_or_else(
                                        || "path".into(),
                                        |path| path.display().to_string()
                                    )
                            ));
                            self.ui.notification_error = false;
                            self.refresh_explorer();
                            self.start_git_status_refresh_silent();
                        }
                        Err(error) => self.show_error(error),
                    }
                }
                VfsResult::CreateTarget { id, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::WorkspaceProposalCreate {
                                proposal,
                                requested_path,
                            }),
                            Ok(path),
                        ) => {
                            let mut finish = false;
                            if let Some(build) = &mut self.pending_workspace_proposal
                                && build.id == proposal
                            {
                                build.resolved_paths.insert(requested_path, path.clone());
                                build.creates.push(path.clone());
                                build.snapshots.push(FileSnapshot {
                                    canonical_path: path,
                                    revision: None,
                                    bytes: Vec::new(),
                                });
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            if finish {
                                self.finish_workspace_proposal();
                            }
                        }
                        (Some(PendingVfs::WorkspaceProposalCreate { .. }), Err(error)) => {
                            if let Some(build) = self.pending_workspace_proposal.take() {
                                self.reject_pending_workspace_proposal(build, error.to_string())
                            }
                        }
                        _ => {}
                    }
                }
                VfsResult::Read { id, path, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    let startup_primary = match &pending {
                        Some(PendingVfs::StartupOpen { primary }) => Some(*primary),
                        _ => None,
                    };
                    if let Some(PendingVfs::PickerPreview {
                        path: requested,
                        generation,
                    }) = &pending
                    {
                        if *generation == self.picker_preview_generation
                            && self.picker_preview_path.as_ref() == Some(requested)
                        {
                            let title = self
                                .picker_preview
                                .as_ref()
                                .map_or_else(String::new, |preview| preview.title.clone());
                            let lines = match result {
                                Ok(bytes) => match std::str::from_utf8(&bytes) {
                                    Ok(text) => Self::picker_preview_lines(text, 512),
                                    Err(_) => vec!["binary preview unavailable".into()],
                                },
                                Err(error) => vec![format!("preview unavailable: {error}")],
                            };
                            self.picker_preview = Some(PickerPreview {
                                title,
                                lines,
                                first_line: 0,
                                highlight_line: None,
                            });
                            self.sync_picker_ui();
                        }
                        continue;
                    }
                    if let Some(PendingVfs::ExplorerPreview {
                        path: requested,
                        generation,
                    }) = &pending
                    {
                        if *generation == self.explorer_preview_generation
                            && self.explorer_preview_path.as_ref() == Some(requested)
                        {
                            self.explorer_preview_lines = match result {
                                Ok(bytes) => {
                                    let maximum = self
                                        .configuration
                                        .settings
                                        .ui
                                        .explorer
                                        .maximum_preview_bytes;
                                    let bytes = &bytes[..bytes.len().min(maximum)];
                                    match std::str::from_utf8(bytes) {
                                        Ok(text) => text
                                            .lines()
                                            .take(
                                                self.configuration
                                                    .settings
                                                    .ui
                                                    .explorer
                                                    .maximum_preview_lines,
                                            )
                                            .map(|line| {
                                                line.chars()
                                                    .filter(|character| {
                                                        !character.is_control()
                                                            || *character == '\t'
                                                    })
                                                    .take(4096)
                                                    .collect()
                                            })
                                            .collect(),
                                        Err(_) => {
                                            vec![self.message("explorer_preview_binary", &[])]
                                        }
                                    }
                                }
                                Err(error) => vec![self.message(
                                    "explorer_preview_failed",
                                    &[("diagnostic", error.to_string())],
                                )],
                            };
                            self.sync_explorer_ui();
                        }
                        continue;
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(PendingVfs::WorkspaceProposalRead {
                                proposal,
                                requested_path,
                            }) => {
                                let mut finish = false;
                                if let Some(build) = &mut self.pending_workspace_proposal
                                    && build.id == proposal
                                {
                                    build.resolved_paths.insert(requested_path, path.clone());
                                    build.snapshots.push(FileSnapshot {
                                        canonical_path: path,
                                        revision: None,
                                        bytes: text.to_bytes(),
                                    });
                                    build.waiting = build.waiting.saturating_sub(1);
                                    finish = build.waiting == 0;
                                }
                                if finish {
                                    self.finish_workspace_proposal();
                                }
                            }
                            Some(PendingVfs::SessionRestore { requested_path }) => {
                                self.accept_session_restore_read(requested_path, path, text)
                            }
                            Some(PendingVfs::Open) => {
                                self.state.open_text(text, Some(path.clone()));
                                self.record_recent_file(path.clone());
                                self.ui.dashboard = false;
                                self.ui.notification = Some(
                                    self.message("opened", &[("path", path.display().to_string())]),
                                );
                                self.ui.notification_error = false;
                            }
                            Some(PendingVfs::LinterRead { generation, raw }) => {
                                let uri = file_uri(&path);
                                let mut diagnostics = Vec::new();
                                for raw in raw {
                                    let start = text
                                        .line_to_byte(raw.line)
                                        .ok()
                                        .map(|offset| {
                                            ByteOffset(
                                                (offset.get() + raw.column).min(text.len_bytes()),
                                            )
                                        })
                                        .filter(|offset| text.validate_offset(*offset).is_ok())
                                        .unwrap_or(ByteOffset(0));
                                    diagnostics.push(Diagnostic {
                                        uri: uri.clone(),
                                        version: None,
                                        range: ByteRange::new(start, start).expect("empty"),
                                        severity: raw.severity,
                                        source: "linter".into(),
                                        code: None,
                                        message: raw.message,
                                        related: Vec::new(),
                                        tags: Vec::new(),
                                        fixes: Vec::new(),
                                        timestamp: crate::clock::system_time(),
                                    });
                                }
                                let mut finish = false;
                                if let Some(build) = self.pending_linter_builds.get_mut(&generation)
                                {
                                    build.grouped.entry(uri).or_default().extend(diagnostics);
                                    build.waiting = build.waiting.saturating_sub(1);
                                    finish = build.waiting == 0;
                                }
                                if finish {
                                    self.finish_linter_build(generation);
                                }
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
                                self.record_recent_file(path);
                                self.ui.dashboard = false;
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
                                self.record_recent_file(path);
                                self.ui.dashboard = false;
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
                                self.ui.dashboard = false;
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
                                self.ui.dashboard = false;
                                self.ensure_cursor_visible();
                            }
                            Some(PendingVfs::ExplorerOpen { split }) => {
                                if split {
                                    self.apply_layout_action(LayoutAction::Split(
                                        crate::editor::Axis::Vertical,
                                    ));
                                }
                                self.state.open_text(text, Some(path));
                                self.ui.dashboard = false;
                            }
                            Some(PendingVfs::StartupOpen { primary }) => {
                                let (buffer, _) = self.state.open_text(text, Some(path.clone()));
                                if primary {
                                    self.startup_primary_buffer = Some(buffer);
                                }
                                self.ui.dashboard = false;
                                self.ui.notification = Some(
                                    self.message("opened", &[("path", path.display().to_string())]),
                                );
                                self.ui.notification_error = false;
                            }
                            Some(PendingVfs::Reload { buffer }) => {
                                if self
                                    .state
                                    .replace_buffer_from_disk(buffer, text, path.clone())
                                {
                                    self.ui.notification = Some(self.message(
                                        "reloaded_file",
                                        &[("path", path.display().to_string())],
                                    ));
                                    self.ui.notification_error = false;
                                } else {
                                    self.show_error(self.message("reload_closed", &[]));
                                }
                            }
                            _ => self.show_error(self.message("unexpected_read", &[])),
                        },
                        Err(error) => {
                            if matches!(pending, Some(PendingVfs::WorkspaceProposalRead { .. })) {
                                if let Some(build) = self.pending_workspace_proposal.take() {
                                    self.reject_pending_workspace_proposal(
                                        build,
                                        error.to_string(),
                                    );
                                }
                            } else {
                                self.show_error(error);
                            }
                        }
                    }
                    if startup_primary.is_some() {
                        self.startup_reads_pending = self.startup_reads_pending.saturating_sub(1);
                        self.finish_startup_reads_if_complete();
                    }
                }
                VfsResult::Directory {
                    id,
                    path: _,
                    result,
                } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::Completion {
                                generation,
                                prompt_input,
                                fragment,
                            }),
                            Ok(paths),
                        ) if generation == self.completion_generation
                            && self
                                .ui
                                .prompt
                                .as_ref()
                                .is_some_and(|prompt| prompt.input == prompt_input) =>
                        {
                            let prefix = PathBuf::from(&fragment)
                                .file_name()
                                .map_or_else(String::new, |name| {
                                    name.to_string_lossy().to_lowercase()
                                });
                            let limit = self.configuration.settings.runtime.completion_limit;
                            let detail =
                                self.configuration.settings.command_labels["file.open"].clone();
                            let workspace = self.state.workspace_root.clone();
                            let select_first =
                                self.configuration.settings.input.completion_select_first;
                            if let Some(prompt) = &mut self.ui.prompt {
                                prompt.completions = paths
                                    .into_iter()
                                    .filter(|path| {
                                        path.file_name().is_some_and(|name| {
                                            name.to_string_lossy()
                                                .to_lowercase()
                                                .starts_with(&prefix)
                                        })
                                    })
                                    .take(limit)
                                    .map(|path| CompletionItem {
                                        label: path
                                            .strip_prefix(&workspace)
                                            .unwrap_or(&path)
                                            .display()
                                            .to_string(),
                                        detail: detail.clone(),
                                    })
                                    .collect();
                                prompt.selected_completion =
                                    (select_first && !prompt.completions.is_empty()).then_some(0);
                            }
                        }
                        (Some(PendingVfs::Completion { .. }), Err(_)) => {}
                        (Some(PendingVfs::Completion { .. }), Ok(_)) => {}
                        (Some(PendingVfs::LinterRead { generation, .. }), Err(error)) => {
                            let mut finish = false;
                            if let Some(build) = self.pending_linter_builds.get_mut(&generation) {
                                build.failed = true;
                                build.waiting = build.waiting.saturating_sub(1);
                                finish = build.waiting == 0;
                            }
                            self.logs.record(
                                LogLevel::Error,
                                "linter",
                                "source-read-failed",
                                BTreeMap::from([("error".into(), error.to_string())]),
                            );
                            if finish {
                                self.finish_linter_build(generation);
                            }
                        }
                        (Some(PendingVfs::SessionRestore { .. }), Err(_)) => {
                            self.fail_session_restore()
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
                VfsResult::Write { id, path, result } => {
                    let pending = self.pending_vfs.remove(&id);
                    match (pending, result) {
                        (
                            Some(PendingVfs::Save {
                                buffer,
                                revision,
                                bytes,
                                target,
                                quit_after,
                            }),
                            Ok(()),
                        ) => {
                            if self.state.mark_buffer_saved_as(
                                buffer,
                                revision,
                                bytes,
                                Some(target),
                            ) {
                                let buffer_state = &self.state.buffers[buffer];
                                let still_dirty = buffer_state.dirty;
                                let lsp_document = buffer_state.path.clone().and_then(|path| {
                                    Some(LspDocument {
                                        language_id: language_id_for_path(&path)?.into(),
                                        path,
                                        version: buffer_state.text.revision().0,
                                        text: buffer_state.text.to_string(),
                                    })
                                });
                                self.ui.notification = Some(if still_dirty {
                                    self.message(
                                        "saved_dirty",
                                        &[
                                            ("revision", revision.0.to_string()),
                                            ("path", path.display().to_string()),
                                        ],
                                    )
                                } else {
                                    self.message("saved", &[("path", path.display().to_string())])
                                });
                                self.ui.notification_error = false;
                                if let Some(server) = self.active_lsp_server.as_deref()
                                    && let Some(document) = lsp_document
                                {
                                    let _ = self.lsp.save(server, document);
                                }
                                if self.ui.explorer.is_some() {
                                    self.start_git_status_refresh_with_feedback(false);
                                }
                                if quit_after && !still_dirty {
                                    if self
                                        .state
                                        .buffers
                                        .iter()
                                        .any(|(_, candidate)| candidate.dirty)
                                    {
                                        self.show_error(self.message("saved_others_dirty", &[]));
                                    } else {
                                        self.should_quit = true;
                                    }
                                }
                            }
                        }
                        (_, Err(error)) => self.show_error(error),
                        _ => self.show_error(self.message("unexpected_completion", &[])),
                    }
                }
            }
        }
    }
}
