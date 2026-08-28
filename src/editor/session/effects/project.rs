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
                self.ui.notification =
                    Some(self.message("workspace_trust_status", &[("state", state)]));
                self.ui.notification_error = false;
            }
            AppEffect::ExplorerOpen => self.open_explorer(),
            AppEffect::ExplorerClose => {
                self.ui.explorer = None;
                self.ui.explorer_focused = false;
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
                self.explorer_hidden = !self.explorer_hidden;
                self.refresh_explorer();
            }
            AppEffect::ExplorerToggleIgnored => {
                self.explorer_ignored = !self.explorer_ignored;
                self.refresh_explorer();
            }
            AppEffect::ExplorerFileOpen { split } => self.explorer_open_file(split),
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
                    self.ui.notification = Some(self.message(
                        "workspace_root_status",
                        &[
                            ("id", root.id.0.to_string()),
                            ("path", root.identity.canonical_path.display().to_string()),
                        ],
                    ));
                    self.ui.notification_error = false;
                }
            }
            AppEffect::WorkspaceSearch(query) => self.start_workspace_search(query, None),
            AppEffect::WorkspaceSearchCancel => {
                self.workspace_search.cancel();
                self.workspace_search_workflow = None;
                self.workspace_search_sequence = 0;
                self.workspace_search_generation = None;
                self.workspace_search_intent = None;
                self.workspace_search_matches.clear();
                self.ui.notification = Some(self.message("workspace_search_cancelled", &[]));
                self.ui.notification_error = false;
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
            AppEffect::WorkspaceEditToggle => {
                let selected = self
                    .picker
                    .as_ref()
                    .and_then(PickerState::selected_item)
                    .and_then(|item| self.picker_payloads.get(&item.id))
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected
                    && let Some(proposal) = &mut self.workspace_proposal
                    && proposal.toggle_file(index).is_ok()
                {
                    self.show_workspace_proposal();
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => {
                let intents = std::mem::take(&mut self.accepted_resource_intents);
                for intent in intents {
                    if self.state.buffers.iter().any(|(_, buffer)| {
                        buffer.path.as_ref() == Some(&intent.source)
                            && (buffer.dirty || buffer.text.to_bytes() != intent.source_bytes)
                    }) {
                        self.show_error(self.message(
                            "workspace_resource_buffer_stale",
                            &[("path", intent.source.display().to_string())],
                        ));
                        continue;
                    }
                    match self.vfs.apply_resource(intent.clone()) {
                        Ok(id) => {
                            self.resource_apply_pending.insert(id, intent);
                        }
                        Err(error) => self.show_error(error),
                    }
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
