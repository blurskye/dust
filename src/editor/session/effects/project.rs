use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn process_project_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceTrust => self.request_workspace_trust(),
            AppEffect::WorkspaceTrustConfirm(phrase) => {
                self.confirm_workspace_trust(phrase);
            }
            AppEffect::WorkspaceUntrust => self.revoke_workspace_trust(),
            AppEffect::WorkspaceStatus => {
                let state = if self.workspace.trust() == WorkspaceTrust::Trusted {
                    self.message("workspace_trust_trusted", &[])
                } else {
                    self.message("workspace_trust_untrusted", &[])
                };
                self.notify_message(
                    self.message("workspace_trust_status", &[("state", state)]),
                    false,
                );
            }
            AppEffect::ExplorerPanelToggle => {
                return self.process_project_effect(if self.ui.explorer.is_some() {
                    AppEffect::ExplorerClose
                } else {
                    AppEffect::ExplorerOpen
                });
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerClose => {
                if self
                    .input
                    .prompt()
                    .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
                {
                    self.close_prompt();
                }
                self.ui.explorer = None;
                self.set_explorer_focus(false);
                self.reconcile_explorer_watch();
                self.ensure_cursor_visible();
            }
            AppEffect::ExplorerMove(delta) => {
                self.move_explorer_selection(isize::from(delta));
            }
            AppEffect::ExplorerPage(direction) => self.page_explorer_selection(direction),
            AppEffect::ExplorerParent => self.explorer_collapse_or_parent(),
            AppEffect::ExplorerChild => {
                self.explorer.select_first_child();
                self.sync_explorer_ui();
            }
            AppEffect::ExplorerToggle => self.explorer_toggle(),
            AppEffect::ExplorerRefresh => self.refresh_explorer(),
            AppEffect::ExplorerFollow => self.follow_active_file_in_explorer(),
            AppEffect::ExplorerToggleHidden => {
                self.explorer.toggle_hidden();
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer.toggle_ignored();
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
            AppEffect::ExplorerCreate(path) => self.create_explorer_path(path),
            AppEffect::ExplorerRename(target) => self.rename_explorer_path(target),
            AppEffect::ExplorerRenamePrompt => self.open_explorer_rename_prompt(),
            AppEffect::ExplorerDeleteRequest => self.request_explorer_delete(),
            AppEffect::ExplorerClipboard { cut } => self.set_explorer_clipboard(cut),
            AppEffect::ExplorerPaste => self.paste_explorer_clipboard(),
            AppEffect::ExplorerYankPath => self.yank_explorer_path(),
            AppEffect::ExplorerMouseSelect => self.explorer_mouse_select(),
            AppEffect::WorkspaceRootAdd(path) => self.add_workspace_root(path),
            AppEffect::WorkspaceRootActivate(id) => {
                self.activate_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootRemove(id) => {
                self.remove_workspace_root(crate::workspace::RootId(id));
            }
            AppEffect::WorkspaceRootStatus => {
                if let Some(root) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.notify_message(
                        self.message(
                            "workspace_root_status",
                            &[
                                ("id", root.id.0.to_string()),
                                ("path", root.identity.canonical_path.display().to_string()),
                            ],
                        ),
                        false,
                    );
                }
            }
            AppEffect::WorkspaceSearchPrompt => self.open_live_workspace_search(),
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.search.cancel_grep();
                self.notify_message(self.message("workspace_search_cancelled", &[]), false);
            }
            AppEffect::WorkspaceReplace { query, replacement } => {
                self.start_workspace_search(query, Some(replacement));
            }
            AppEffect::OpenPicker(kind) => self.open_builtin_picker(kind),
            AppEffect::PickerInsert(text) => self.picker_insert(&text),
            AppEffect::PickerBackspace => self.picker_backspace(),
            AppEffect::PickerMove(direction) => self.picker_move(direction),
            AppEffect::PickerAccept => self.accept_picker(),
            AppEffect::PickerCancel => self.close_picker(),
            AppEffect::PickerNormal | AppEffect::PickerInsertMode => {
                if self.search.picker().is_some() && self.search.workspace_picker().is_none() {
                    self.input
                        .set_picker_normal(matches!(effect, AppEffect::PickerNormal));
                    self.sync_picker_ui();
                }
            }
            AppEffect::WorkspaceEditToggle => {
                self.retire_stale_picker();
                let selected = self
                    .selected_picker_payload()
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected
                    && self.workspace_edits.toggle(index).is_ok()
                {
                    self.show_workspace_proposal();
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => loop {
                let result = match self.workspace_edits.prepare_resource(
                    &self.workspace,
                    self.configuration.binding_revision_ref(),
                    &self.state,
                ) {
                    Ok(Some(permit)) => self.documents.submit_workspace_resource(permit),
                    Ok(None) => break,
                    Err(error) => {
                        self.show_error(error);
                        break;
                    }
                };
                if let Err(error) = result {
                    self.show_error(error);
                    break;
                }
            },
            effect => return Err(effect),
        }
        Ok(())
    }
}

use crate::editor::session::search::{Intent, SearchUpdate};
impl InteractiveApplication {
    pub(in crate::editor::session) fn open_live_workspace_search(&mut self) {
        self.close_picker();
        self.search.open_live();
        self.install_surface_picker_state(
            PickerSurfaceKind::Generic,
            self.message("workspace_search_title", &[]),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::Grep,
        );
    }
    pub(in crate::editor::session) fn restart_live_workspace_search(&mut self, query: String) {
        self.search.set_query(query.clone());
        if query.is_empty() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                Vec::new(),
                query,
                true,
                PickerOrigin::Grep,
            );
        } else {
            self.start_workspace_search(query, None);
        }
    }
    pub(in crate::editor::session) fn start_workspace_file_picker(&mut self) {
        self.close_picker();
        let configured = &self.configuration.settings().workspace.search;
        let root_revision = self.workspace.root_revision();
        let policy = WorkspaceFilePolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect(),
            root_revision,
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_path_bytes: 16 * 1024 * 1024,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.search.start_files(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                "Files".into(),
                Vec::new(),
                String::new(),
                false,
                PickerOrigin::Files,
            ),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn process_workspace_file_events(&mut self) {
        let Some(result) = self.search.poll_files(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.close_picker();
                self.show_error(error);
                return;
            }
        };
        let multi_root = self.workspace.roots().len() > 1;
        let items = entries
            .into_iter()
            .map(|entry| {
                (
                    PickerItem {
                        id: PickerItemId(entry.id),
                        label: entry.relative_path.display().to_string(),
                        detail: String::new(),
                        source: if multi_root {
                            entry.root.file_name().map_or_else(String::new, |name| {
                                name.to_string_lossy().into_owned()
                            })
                        } else {
                            String::new()
                        },
                        action: PickerAction::None,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: entry.canonical_path,
                        line: 0,
                        column: 0,
                    }),
                )
            })
            .collect();
        match compose_workspace_file_items(items) {
            Ok(items) => self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                "Files".into(),
                items,
                String::new(),
                !self.search.files_pending(),
                PickerOrigin::Files,
            ),
            Err(error) => self.show_error(error.to_string()),
        }
    }
    pub(in crate::editor::session) fn start_workspace_search(
        &mut self,
        query: String,
        replacement: Option<String>,
    ) {
        let configured = &self.configuration.settings().workspace.search;
        let policy = WorkspaceSearchPolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            query,
            regex: configured.regex,
            smart_case: configured.smart_case,
            whole_word: configured.whole_word,
            include_globs: configured.include_globs.clone(),
            exclude_globs: configured.exclude_globs.clone(),
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: configured.maximum_files,
            maximum_results: configured.maximum_results,
            maximum_file_bytes: configured.maximum_file_bytes,
            maximum_total_bytes: configured.maximum_total_bytes,
            maximum_preview_bytes: configured.maximum_preview_bytes,
            batch_size: configured.batch_size,
            event_queue_capacity: configured.event_queue_capacity,
        };
        match self.search.start(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
            replacement,
        ) {
            Ok(()) => {
                if self.search.live() {
                    self.install_surface_picker_state(
                        PickerSurfaceKind::Generic,
                        self.message("workspace_search_title", &[]),
                        Vec::new(),
                        self.search.query().into(),
                        false,
                        PickerOrigin::Grep,
                    );
                    self.clear_notice();
                } else {
                    self.close_picker();
                    self.notify_message(self.message("workspace_search_started", &[]), false);
                }
            }
            Err(error) => {
                self.show_error(self.message("workspace_search_failed", &[("diagnostic", error)]))
            }
        }
    }
    pub(in crate::editor::session) fn process_workspace_search_events(&mut self) {
        for update in self.search.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            match update {
                SearchUpdate::Batch => {
                    if self.search.live() {
                        self.open_workspace_search_picker();
                    }
                }
                SearchUpdate::Failed(error) => {
                    self.close_picker();
                    self.show_error(
                        self.message("workspace_search_failed", &[("diagnostic", error)]),
                    );
                }
                SearchUpdate::Complete {
                    intent: Intent::Find,
                    files,
                } => {
                    if self.search.live() || !self.search.matches().is_empty() {
                        self.open_workspace_search_picker();
                        if self.search.live() {
                            self.clear_notice();
                        } else {
                            self.notify_message(
                                self.message(
                                    "workspace_search_complete",
                                    &[
                                        ("count", self.search.matches().len().to_string()),
                                        ("files", files.to_string()),
                                    ],
                                ),
                                false,
                            );
                        }
                    } else {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    }
                }
                SearchUpdate::Complete {
                    intent: Intent::Replace(replacement),
                    ..
                } => {
                    if self.search.matches().is_empty() {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    } else {
                        self.begin_workspace_search_replacement(replacement);
                    }
                }
            }
        }
    }
    pub(in crate::editor::session) fn open_workspace_search_picker(&mut self) {
        let Some(order) = self
            .search
            .workflow()
            .and_then(|broker| broker.ordered_items())
        else {
            self.show_error("workspace search workflow projection is unavailable");
            return;
        };
        let matches = self
            .search
            .matches()
            .iter()
            .map(|found| (found.id.as_str(), found))
            .collect::<BTreeMap<_, _>>();
        let multi_root = self.workspace.roots().len() > 1;
        let items = order
            .into_iter()
            .filter_map(|item| matches.get(item.stable_key.as_str()).copied())
            .map(|found| {
                let item = PickerItem {
                    id: PickerItemId(found.id.clone()),
                    label: self.message(
                        "workspace_search_row",
                        &[
                            ("path", found.relative_path.display().to_string()),
                            ("line", found.line.saturating_add(1).to_string()),
                            ("column", found.column.saturating_add(1).to_string()),
                        ],
                    ),
                    detail: found.line_text.trim().to_owned(),
                    source: if multi_root {
                        found
                            .root
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                    } else {
                        String::new()
                    },
                    action: PickerAction::None,
                };
                (item, PickerPayload::WorkspaceSearch(found.id.clone()))
            })
            .collect();
        if self.search.live() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                items,
                self.search.query().into(),
                !self.search.pending(),
                PickerOrigin::Grep,
            );
        } else {
            self.install_surface_picker_state(
                PickerSurfaceKind::Generic,
                self.message("workspace_search_title", &[]),
                items,
                String::new(),
                true,
                PickerOrigin::Grep,
            );
        }
    }
    pub(in crate::editor::session) fn begin_workspace_search_replacement(
        &mut self,
        replacement: String,
    ) {
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let mut grouped = BTreeMap::<PathBuf, (Vec<u8>, Vec<TextEdit>)>::new();
        for found in self.search.matches() {
            let entry = grouped
                .entry(found.canonical_path.clone())
                .or_insert_with(|| (found.snapshot.to_vec(), Vec::new()));
            if entry.0.as_slice() != found.snapshot.as_ref() {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            }
            let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            else {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            };
            entry.1.push(TextEdit::new(range, replacement.clone()));
        }
        let mut snapshots = Vec::with_capacity(grouped.len());
        let mut drafts = Vec::with_capacity(grouped.len());
        for (path, (searched_bytes, edits)) in grouped {
            let revision = if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                if buffer.text.to_bytes() != searched_bytes {
                    self.show_error(self.message("workspace_search_stale_result", &[]));
                    return;
                }
                Some(buffer.text.revision())
            } else {
                None
            };
            snapshots.push(FileSnapshot {
                canonical_path: path.clone(),
                revision,
                bytes: searched_bytes,
            });
            drafts.push(WorkspaceEditDraft {
                canonical_path: path,
                expected_revision: revision,
                edits,
            });
        }
        let id = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(id) => id,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match self.workspace_edits.build(
            crate::editor::session::workspace_edits::PreparedEdits {
                ticket: id,
                label: self.message("workspace_replace_label", &[]),
                drafts,
                snapshots,
                resources: Vec::new(),
            },
            self.workspace.edit_scope(),
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => {
                self.show_workspace_proposal();
            }
            Err(error) => {
                self.workspace_edits.reject();
                self.show_error(self.message("workspace_edit_failed", &[("diagnostic", error)]));
            }
        }
    }
}

impl InteractiveApplication {
    pub(in crate::editor::session) fn explorer_open_file(&mut self, split: bool) {
        if self
            .explorer
            .tree()
            .selected_node()
            .is_some_and(|node| matches!(node.kind, ExplorerKind::Root | ExplorerKind::Directory))
        {
            self.explorer_toggle();
            return;
        }
        if let Err(error) = self.documents.check_admission() {
            self.show_error(error);
            return;
        }
        let request = self.explorer.prepare_open(
            split,
            (
                self.workspace.version(),
                self.configuration.binding_revision(),
            ),
            self.state.window_version(),
            self.workspace.roots(),
        );
        let (retired, request) = match request {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        if let Some(id) = retired {
            self.documents.cancel_request(id);
        }
        match self.documents.open_explorer(request.path, request.roots) {
            Ok(id) => {
                self.set_explorer_focus(false);
                // Finish the requested focus change before capturing its async
                // navigation guard; a redraw must not look like new navigation.
                self.reconcile_editor_windows();
                let navigation = self
                    .window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view);
                if !self.explorer.admit_open(request.ticket, id, navigation) {
                    self.documents.cancel_request(id);
                }
            }
            Err(error) => {
                self.explorer.fail_open(request.ticket);
                self.show_error(error);
            }
        }
    }
    pub(in crate::editor::session) fn cancel_explorer_confirmation(&mut self) {
        if let Some(id) = self.explorer.cancel_confirmation() {
            self.documents.cancel_request(id);
        }
    }
    pub(in crate::editor::session) fn retire_stale_explorer_intents(&mut self) {
        let (ids, may_write) = self.explorer.retire_stale_intents(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        let mut all_cancelled = true;
        for id in ids {
            all_cancelled &= self.documents.cancel_request(id);
        }
        if may_write && !all_cancelled {
            self.notify_message(
                "explorer source changed; a running filesystem write may still finish".into(),
                true,
            );
        }
        if self
            .input
            .prompt()
            .is_some_and(|p| p.kind == PromptKind::ExplorerRename)
            && !self.explorer.rename_current(
                &self.workspace.version(),
                self.configuration.binding_revision(),
                &self.state.window_version(),
            )
        {
            self.close_prompt();
        }
        if !self.explorer.confirmation_pending()
            && self
                .input
                .prompt()
                .is_some_and(|p| p.kind == PromptKind::ExplorerDelete)
        {
            self.close_prompt();
        }
    }
    pub(in crate::editor::session) fn retire_explorer_intents(&mut self) {
        let (ids, may_write) = self.explorer.retire_intents();
        let mut all_cancelled = true;
        for id in ids {
            all_cancelled &= self.documents.cancel_request(id);
        }
        if may_write && !all_cancelled {
            self.notify_message(
                "explorer operation retired; a running filesystem write may still finish".into(),
                true,
            );
        }
    }
    fn queue_explorer_mutation(
        &mut self,
        operation: Result<ExplorerMutation, String>,
        confirmation: bool,
        rename_prompt: bool,
    ) -> Result<(), String> {
        let operation = operation?;
        if self.explorer.mutation_pending() {
            return Err("an explorer operation is already pending".into());
        }
        self.documents
            .check_admission()
            .map_err(|error| error.to_string())?;
        let change = match &operation {
            ExplorerMutation::Rename { source, .. } | ExplorerMutation::Delete { path: source } => {
                Some(self.state.capture_path_change(source)?)
            }
            _ => None,
        };
        let context = crate::editor::session::explorer::MutationContext {
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
            editor: self.state.window_version(),
            change,
        };
        let request = self.explorer.prepare_mutation(
            operation,
            context,
            self.workspace.roots(),
            self.configuration
                .settings()
                .workspace
                .search
                .maximum_file_bytes,
            confirmation,
        )?;
        let result = if rename_prompt {
            let source = self.explorer.submitted_rename_source();
            let target = request
                .operation
                .target()
                .expect("validated rename target")
                .into();
            self.documents.prepare_explorer_rename(source, target)
        } else {
            self.documents
                .prepare_explorer(request.operation, request.roots, request.maximum)
        }
        .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_mutation(request.ticket, result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            return Err(error);
        }
        self.notify_message("checking explorer operation…".into(), false);
        Ok(())
    }
    pub(in crate::editor::session) fn create_explorer_path(&mut self, input: String) {
        if let Err(error) = self.queue_explorer_mutation(self.explorer.create(&input), false, false)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn rename_explorer_path(&mut self, target: String) {
        if let Err(error) =
            self.queue_explorer_mutation(self.explorer.rename(&target), false, false)
        {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn open_explorer_rename_prompt(&mut self) {
        self.close_prompt();
        let request = match self.explorer.begin_rename(
            self.workspace.version(),
            self.configuration.binding_revision(),
            self.state.window_version(),
            self.workspace.roots(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let result = self.explorer.rename_inspection().and_then(|inspection| {
            let id = self
                .documents
                .inspect_explorer_rename(
                    request.path,
                    request.roots,
                    self.configuration
                        .settings()
                        .workspace
                        .search
                        .maximum_file_bytes,
                )
                .map_err(|error| error.to_string())?;
            inspection.register(id);
            Ok(())
        });
        if let Err(error) = result {
            self.explorer.cancel_rename();
            self.show_error(error);
            return;
        }
        self.open_prompt(PromptKind::ExplorerRename, request.initial);
        self.prompt_help(format!(
            "Relative to `{}`. Checking source… Enter renames; Esc cancels.",
            request.root.display()
        ));
    }
    pub(in crate::editor::session) fn submit_explorer_rename_prompt(&mut self, value: &str) {
        let operation = self.explorer.rename_value(
            value,
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        );
        if let Err(error) = self.queue_explorer_mutation(operation, false, true) {
            self.prompt_error(error);
        } else {
            self.prompt_help("Checking rename target… Esc cancels before application.".into());
        }
    }
    pub(in crate::editor::session) fn cancel_explorer_rename_prompt(&mut self) {
        for id in self.explorer.cancel_rename() {
            self.documents.cancel_request(id);
        }
    }
    pub(in crate::editor::session) fn request_explorer_delete(&mut self) {
        self.close_prompt();
        if let Err(error) = self.queue_explorer_mutation(self.explorer.delete(), true, false) {
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn confirm_explorer_delete(&mut self) {
        match self.explorer.confirm_delete(
            self.workspace.version(),
            self.configuration.binding_revision(),
            &self.state.window_version(),
        ) {
            Ok(prepared) => self.apply_prepared_explorer(prepared),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn apply_prepared_explorer(
        &mut self,
        prepared: crate::files::PreparedExplorerMutation,
    ) {
        let result = self
            .documents
            .apply_explorer(prepared)
            .map_err(|e| e.to_string());
        let id = result.as_ref().ok().copied();
        if let Err(error) = self.explorer.admit_apply(result) {
            if let Some(id) = id {
                self.documents.cancel_request(id);
            }
            self.show_error(error);
        }
    }
    pub(in crate::editor::session) fn set_explorer_clipboard(&mut self, cut: bool) {
        match self.explorer.clipboard(cut, self.workspace.version()) {
            Ok(message) => self.notify_message(message, false),
            Err(error) => self.show_error(error),
        }
    }
    pub(in crate::editor::session) fn paste_explorer_clipboard(&mut self) {
        if let Err(error) = self.queue_explorer_mutation(
            self.explorer.paste(&self.workspace.version()),
            false,
            false,
        ) {
            self.show_error(error);
        }
    }
}
