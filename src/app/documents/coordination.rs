use crate::app::ByteOffset;
use crate::app::ByteRange;
use crate::app::EditorOperation;
use crate::app::EditorTransaction;
use crate::app::Instant;
use crate::app::App;
use crate::app::PositionCodec;
use crate::app::Precondition;
use crate::app::RestoreContext;
use crate::app::TextBuffer;
use crate::app::UndoPolicy;
use crate::app::DocumentEvent;
use crate::app::InteractiveError;
use crate::app::RestoreUpdate;
use crate::app::VfsResult;
use crate::app::command;
use std::ops::ControlFlow::Break;
use std::ops::ControlFlow::Continue;

use crate::app::language::interaction::NavigationRead;



/// Lines of a recovery journal its preview shows.
pub(in crate::app) const RECOVERY_PREVIEW_LINES: usize = 500;
impl App {

    pub(in crate::app) fn effective_large_file(&self, buffer: crate::editor::BufferId) -> bool {
        self.documents.effective_large_file(
            buffer,
            self.state.buffers.get(buffer).map(|buffer| &buffer.text),
            &self.configuration.settings().editor.large_file,
        )
    }
}

impl App {

    pub(in crate::app) fn process_large_file_modes(&mut self) {
        let large = self.documents.reconcile_policy(
            &self.state.window_version(),
            self.state
                .buffers
                .iter()
                .map(|(id, buffer)| (id, buffer.path.as_deref(), &buffer.text)),
            &self.configuration.settings().editor.large_file,
        );
        self.project_large_file_modes(large);
    }
}

impl App {


    pub(in crate::app) fn sync_status_summary_ui(&mut self) {
        let branch = self.git.branch().map(str::to_owned);
        self.ui.set(branch, |ui| &mut ui.git_branch);
        let running = self.tasks.running_count();
        self.ui.set(running, |ui| &mut ui.running_tasks);
    }
}

impl App {


    pub(in crate::app) fn toggle_buffer_pin(&mut self, buffer: crate::editor::BufferId) {
        if !self.state.buffers.contains_key(buffer) {
            return;
        }
        self.window_session.toggle_pin(buffer);
        self.reconcile_editor_windows();
    }
}

impl App {


    pub(in crate::app) fn toggle_active_buffer_pin(&mut self) {
        if let Some(buffer) = self.state.active_buffer_id() {
            self.toggle_buffer_pin(buffer)
        }
    }
}

impl App {


    pub(in crate::app) fn move_active_buffer_line(&mut self, direction: isize) {
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        if let Err(error) = self.window_session.move_content(buffer, direction) {
            self.show_error(error);
            return;
        }
        self.reconcile_editor_windows();
    }
}

impl App {

    pub(in crate::app) fn schedule_recovery_work(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            false,
            self.documents.recovery_io(),
        );
        if std::mem::take(&mut self.recovery.oversized_notice) {
            self.notify_warning(self.message("recovery_too_large", &[]));
        }
    }
}

impl App {

    pub(in crate::app) fn queue_dirty_recovery_before_exit(&mut self) {
        self.recovery.schedule(
            &self.state,
            &self.configuration.settings().recovery,
            Instant::now(),
            true,
            self.documents.recovery_io(),
        );
    }
}

impl App {


    pub(in crate::app) fn process_vfs_results(&mut self) {
        self.process_pending_saves();
        loop {
            self.submit_restore_reads();
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
                    self.process_pending_saves();
                    continue;
                }
                Ok(Some(DocumentEvent::Stopped {
                    requests,
                    error,
                    task_reads,
                })) => {
                    for id in requests {
                        self.input.retire_document_request(id);
                        self.search.retire_document_request(id);
                        self.language.retire_document_request(id);
                        self.persistence.retire_document_request(id);
                        self.navigation_reads.remove(&id);
                        self.recovery.take_read(id, self.workspace.version());
                    }
                    self.fail_agent_files(&error);
                    self.recovery.stopped();
                    self.search.clear_preview();
                    self.explorer.transport_stopped();
                    self.persistence.restore_transport_stopped();
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
                        Ok(buffer) => Some(buffer),
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
            let result = match self.persistence.accept_restore(
                result,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) {
                Break(RestoreUpdate::Waiting) => continue,
                Break(RestoreUpdate::Ready(ready)) => {
                    self.finish_session_restore(*ready);
                    continue;
                }
                Break(RestoreUpdate::Abandoned) => {
                    self.cancel_session_restore();
                    continue;
                }
                Break(RestoreUpdate::Failed(error)) => {
                    self.cancel_session_restore();
                    self.show_error(error);
                    continue;
                }
                Continue(result) => result,
            };
            let Some(result) = self.accept_agent_file(result) else {
                continue;
            };
            let result = match self.recovery.accept(result) {
                Break(update) => {
                    use crate::app::documents::recovery::RecoveryUpdate;
                    match update {
                        RecoveryUpdate::Quiet | RecoveryUpdate::Pruned(0) => {}
                        RecoveryUpdate::Error(error) => self.show_error(error),
                        RecoveryUpdate::Pruned(count) => self.notify_message(
                            self.message("recovery_pruned", &[("count", count.to_string())]),
                            false,
                        ),
                        RecoveryUpdate::Discarded => {
                            if self.recovery_picker_open() {
                                self.open_recovery_picker();
                            }
                            self.notify_message(self.message("recovery_discarded", &[]), false);
                        }
                        RecoveryUpdate::CrashCleared => {
                            self.notify_message(self.message("crash_cleared", &[]), false)
                        }
                    }
                    continue;
                }
                Continue(result) => result,
            };
            match result {
                VfsResult::PathsResolved { .. }
                | VfsResult::RecoveryDiscovered { .. }
                | VfsResult::RecoveryCleaned { .. }
                | VfsResult::RecoveryWritten { .. } => {
                    self.show_error(self.message("unexpected_completion", &[]))
                }
                VfsResult::Deleted { result, .. } => match result {
                    Err(error) => self.show_error(error),
                    Ok(()) => self.show_error(self.message("unexpected_completion", &[])),
                },
                VfsResult::ExplorerMutated { .. } => {
                    self.show_error("unowned explorer mutation result")
                }
                VfsResult::ResourceApplied { .. }
                | VfsResult::ResourceResolved { .. }
                | VfsResult::CreateTarget { .. } => self.show_error("unowned workspace I/O result"),
                VfsResult::Read { id, path, result } => {
                    if self.search.accept_preview(
                        id,
                        &result,
                        self.workspace.version(),
                        self.configuration.binding_revision(),
                    ) {
                        self.sync_picker_ui();
                        continue;
                    }
                    let pending = self
                        .search
                        .take_matcher_navigation(id)
                        .map(NavigationRead::Matcher)
                        .or_else(|| {
                            self.persistence
                                .take_navigation_read(id)
                                .map(NavigationRead::Persisted)
                        })
                        .or_else(|| {
                            self.language.take_navigation_read(id).map(
                                |(range, encoding, navigation, workspace)| {
                                    NavigationRead::Lsp(range, encoding, navigation, workspace)
                                },
                            )
                        })
                        .or_else(|| {
                            self.search
                                .take_byte_navigation(id)
                                .map(|(range, snapshot)| NavigationRead::ByteRange(range, snapshot))
                        })
                        .or_else(|| {
                            self.recovery.take_read(id, self.workspace.version()).map(
                                |(journal, content)| NavigationRead::Recovered(journal, content),
                            )
                        });
                    if let Some((navigation, capture)) = self.navigation_reads.remove(&id)
                        && (!self.window_session.navigation_current(
                            &navigation,
                            &self.state.window_version(),
                            self.state.active_view,
                        ) || !self.input.capture_current(&capture))
                    {
                        continue;
                    }
                    if let Some(NavigationRead::Recovered(journal, content)) = &pending {
                        // A read may finish after another command opened this file. Never
                        // replace that buffer, and never bind a rejected path to recovered text.
                        let already_open = self
                            .state
                            .buffers
                            .values()
                            .any(|buffer| buffer.path.as_ref() == Some(&path));
                        let missing = matches!(&result, Err(crate::files::VfsError::Io {
                            operation: "open for reading" | "open scoped path", source, ..
                        }) if source.kind() == std::io::ErrorKind::NotFound);
                        let writable = self.workspace.require_path(&path).is_ok();
                        let (text, destination) = match result {
                            Ok(bytes) if !already_open && writable => {
                                match TextBuffer::from_bytes(&bytes) {
                                    Ok(text) => (text, Some(path)),
                                    Err(_) => (TextBuffer::from_text(""), None),
                                }
                            }
                            Err(_) if missing && !already_open && writable => {
                                (TextBuffer::from_text(""), Some(path))
                            }
                            _ => (TextBuffer::from_text(""), None),
                        };
                        if let Some(path) = &destination {
                            self.record_recent_file(path.clone());
                        }
                        let (buffer, _) = if missing && let Some(path) = destination.as_ref() {
                            let mut candidate = self.state.draft();
                            let opened = match candidate.open_new_document(path.clone()) {
                                Ok(opened) => opened,
                                Err(error) => {
                                    self.show_error(error);
                                    continue;
                                }
                            };
                            if let Err(error) = self.commit_editor_draft(candidate, None, true) {
                                self.show_error(error);
                                continue;
                            }
                            opened
                        } else {
                            match self.state.open_text(text, destination) {
                                Ok(opened) => opened,
                                Err(error) => {
                                    self.show_error(error);
                                    continue;
                                }
                            }
                        };
                        if let Err(error) = self.replace_with_recovered(buffer, content.clone()) {
                            self.show_error(error);
                            continue;
                        }
                        self.finish_recovery(journal, "recovery_restored");
                        continue;
                    }
                    if let Some(NavigationRead::ByteRange(_, Some(snapshot))) = &pending {
                        let current = self
                            .state
                            .buffers
                            .values()
                            .find(|buffer| buffer.path.as_ref() == Some(&path));
                        let unchanged = current.map_or_else(
                            || {
                                result
                                    .as_deref()
                                    .is_ok_and(|bytes| bytes == snapshot.as_ref())
                            },
                            |buffer| buffer.text.matches_encoded_text(snapshot),
                        );
                        if !unchanged {
                            self.show_error(self.message("workspace_search_stale_result", &[]));
                            continue;
                        }
                    }
                    match result.map_err(InteractiveError::from).and_then(|bytes| {
                        TextBuffer::from_bytes(&bytes).map_err(InteractiveError::from)
                    }) {
                        Ok(text) => match pending {
                            Some(NavigationRead::Matcher(source)) => {
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
                                let start = PositionCodec::new(&self.state.buffers[buffer].text)
                                    .lsp_to_byte(source.line, source.column, source.encoding)
                                    .unwrap_or(ByteOffset(0));
                                self.state.views[view].cursor = start;
                                self.state.mark_editor_changed();
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Persisted(location)) => {
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                let length = self.state.buffers[buffer].text.len_bytes();
                                let clamp = |offset: usize| {
                                    let mut offset = offset.min(length);
                                    while !self.state.buffers[buffer]
                                        .text
                                        .is_char_boundary(ByteOffset(offset))
                                    {
                                        offset -= 1;
                                    }
                                    ByteOffset(offset)
                                };
                                let cursor = clamp(location.byte);
                                let anchor = clamp(location.anchor);
                                let selection =
                                    (anchor != cursor).then_some(crate::editor::Selection {
                                        anchor,
                                        head: cursor,
                                        kind: crate::editor::SelectionKind::Character,
                                    });
                                let transaction = EditorTransaction::new(self.transaction_id())
                                    .with_operation(EditorOperation::SetCursor {
                                        view,
                                        position: cursor,
                                    })
                                    .with_operation(EditorOperation::SetSelection {
                                        view,
                                        selection,
                                    })
                                    .with_undo_policy(UndoPolicy::Ignore);
                                if let Err(error) = self.state.apply_transaction(&transaction) {
                                    self.show_error(error);
                                    continue;
                                }
                                self.state.views[view].scroll_line = location.scroll_line.min(
                                    self.state.buffers[buffer]
                                        .text
                                        .len_lines()
                                        .saturating_sub(1),
                                );
                                self.state.views[view].scroll_wrap_row = 0;
                                self.state.views[view].scroll_column = location.scroll_column;
                                self.record_recent_file(path);
                                self.set_dashboard_visibility(false);
                                self.ensure_cursor_visible();
                            }
                            Some(NavigationRead::Lsp(range, encoding, navigation, workspace)) => {
                                if workspace != self.workspace.version() {
                                    continue;
                                }
                                if !self.window_session.navigation_current(
                                    &navigation,
                                    &self.state.window_version(),
                                    self.state.active_view,
                                ) {
                                    let mut draft = self.state.draft();
                                    let buffer = draft.load_document(text, path.clone());
                                    if let Err(error) = self.commit_editor_draft(draft, None, false)
                                    {
                                        self.show_error(error);
                                    }
                                    if let Some(buffer) = self.state.buffers.get_mut(buffer) {
                                        buffer.read_only =
                                            self.workspace.require_path(&path).is_err();
                                    }
                                    continue;
                                }
                                let (buffer, view) =
                                    match self.state.open_text(text, Some(path.clone())) {
                                        Ok(opened) => opened,
                                        Err(error) => {
                                            self.show_error(error);
                                            continue;
                                        }
                                    };
                                self.state.buffers[buffer].read_only =
                                    self.workspace.require_path(&path).is_err();
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
                            Some(NavigationRead::ByteRange(range, snapshot)) => {
                                let start = range.start;
                                let end = range.end;
                                let target = self
                                    .state
                                    .buffers
                                    .values()
                                    .find(|buffer| buffer.path.as_ref() == Some(&path))
                                    .map_or(&text, |buffer| &buffer.text);
                                if snapshot.as_ref().is_some_and(|snapshot| {
                                    !text.matches_encoded_text(snapshot)
                                        || !target.matches_encoded_text(snapshot)
                                }) || target.validate_offset(start).is_err()
                                    || target.validate_offset(end).is_err()
                                {
                                    self.show_error(
                                        self.message("workspace_search_stale_result", &[]),
                                    );
                                    continue;
                                }
                                let (buffer, view) = match self.state.open_text(text, Some(path)) {
                                    Ok(opened) => opened,
                                    Err(error) => {
                                        self.show_error(error);
                                        continue;
                                    }
                                };
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
                    let pending = self.input.take_completion_read(id);
                    match (pending, result) {
                        (Some(key), _) if !self.input.completion_current(&key) => {
                            self.refresh_completions();
                        }
                        (Some(key), Ok(paths)) => {
                            self.input.accept_prompt_paths(
                                &key,
                                paths,
                                &self.state.workspace_root,
                                self.configuration.settings(),
                            );
                            self.project_input_prompt();
                        }
                        (Some(_), Err(_)) => {}
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
        if self.search.preview_needs_retry() && self.documents.check_admission().is_ok() {
            self.sync_picker_ui();
        }
        if self.workspace_edits.preparing() {
            self.submit_workspace_preparation();
        }
    }
}

impl App {

    pub(super) fn project_large_file_modes(&mut self, buffers: Vec<crate::editor::BufferId>) {
        self.ui.set(buffers, |ui| &mut ui.large_file_buffers);
    }
}
