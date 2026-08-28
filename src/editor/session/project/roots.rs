use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn add_workspace_root(&mut self, path: PathBuf) {
        if self.pending_root_operation.is_some() {
            self.show_error(self.message("workspace_root_operation_busy", &[]));
            return;
        }
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.trust_service.inspect_root(requested_path.clone()) {
            Ok(generation) => {
                self.pending_root_operation = Some(PendingRootOperation {
                    generation,
                    root_revision: self.workspace.root_revision(),
                    requested_path,
                });
                self.ui.notification = Some(self.message("workspace_root_loading", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn root_removal_blocker(
        &self,
        root: crate::workspace::RootId,
    ) -> Option<String> {
        let root = self
            .workspace
            .roots()
            .iter()
            .find(|candidate| candidate.id == root)?;
        if self.state.buffers.values().any(|buffer| {
            buffer.dirty
                && buffer
                    .path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&root.identity.canonical_path))
        }) {
            return Some(self.message("workspace_root_dirty_blocker", &[]));
        }
        let busy = self.pending_root_operation.is_some()
            || self.active_lsp_server.is_some()
            || self
                .lsp_sessions
                .values()
                .any(|state| state.restart_due.is_some())
            || self.dap.is_running()
            || self
                .tools
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing { .. }))
            || self.workspace_search_generation.is_some()
            || self.pending_workspace_proposal.is_some()
            || self.workspace_proposal.is_some()
            || !self.pending_vfs.is_empty()
            || self.trust_persist_generation.is_some()
            || self.execution_validation.is_some()
            || !self.startup_reads.is_empty()
            || self.startup_reads_pending > 0;
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }

    pub(in crate::editor::session) fn invalidate_root_scoped_operations(&mut self) {
        self.clear_git_status();
        for terminal in self.terminals.keys().copied().collect::<Vec<_>>() {
            let _ = self.ptys.close(terminal);
        }
        self.active_terminal = None;
        self.terminal_focused = false;
        if !self.terminals.is_empty() {
            self.terminals.clear();
            self.terminal_set_revision = self.terminal_set_revision.saturating_add(1);
        }
        self.ui.terminal_panel = None;
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .lsp_sessions
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.lsp.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.lsp_sessions.remove(&key);
            self.lsp_session_synced
                .retain(|(server, _), _| server != &key);
            self.lsp_synced_revisions.clear();
            if self.active_lsp_server.as_deref() == Some(&key) {
                self.active_lsp_server = None;
                self.lsp_capabilities = None;
                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.pending_execution_preflight.clear();
        self.registry_intents.clear();
        self.pending_registry_artifact = None;
        for (plugin, (load, token)) in self.package_prepared.drain().collect::<Vec<_>>() {
            let _ = self.plugins.unload_generation(&plugin, load);
            self.remove_plugin_contribution_generation(&plugin, load);
            let _ = self.packages.request(PackageOperation::Abort { token });
        }
        for (plugin, load) in self.package_commits.values().cloned().collect::<Vec<_>>() {
            let _ = self.plugins.unload_generation(&plugin, load);
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        let _ = self.dap.stop();
        self.dap_responses.clear();
        self.dap_console.clear();
        self.dap_terminal_responses.clear();
        self.active_debug_terminal_profile = None;
        self.active_debug_configuration = None;
        self.dap_breakpoints.clear();
        self.dap_watches.clear();
        self.snippet_session = None;
        for name in self.task_status.keys() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.task_status.clear();
        self.pending_linter_builds.clear();
        self.task_output.clear();
        self.task_output_rows.clear();
        self.test_results.clear();
        self.task_truncated.clear();
        for job in self.job_tasks.keys().copied().collect::<Vec<_>>() {
            let _ = self.jobs.cancel(job);
        }
        self.clear_git_status();
        self.workspace_files.cancel();
        self.workspace_file_generation = None;
        self.workspace_file_root_revision = None;
        self.workspace_file_entries.clear();
        self.workspace_search.cancel();
        self.workspace_search_workflow = None;
        self.workspace_search_sequence = 0;
        self.workspace_search_generation = None;
        self.workspace_search_intent = None;
        self.workspace_search_matches.clear();
        self.workspace_search_root_revision = None;
        if self.pending_workspace_proposal.is_some() || self.workspace_proposal.is_some() {
            self.reject_workspace_proposal();
        }
        self.workspace_proposal_root_revision = None;
        self.workspace_resource_intents.clear();
        self.accepted_resource_intents.clear();
        self.resource_apply_pending.clear();
        self.explorer_pending = None;
        self.explorer_preview_path = None;
        self.explorer_preview_lines.clear();
        self.explorer_preview_generation = self.explorer_preview_generation.saturating_add(1);
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.ui.explorer_focused = false;
        self.reconcile_explorer_watch();
        self.close_picker();
        self.ensure_cursor_visible();
    }

    pub(in crate::editor::session) fn remove_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                self.invalidate_root_scoped_operations();
                self.ui.notification = Some(self.message(
                    "workspace_root_removed",
                    &[("path", root.identity.canonical_path.display().to_string())],
                ));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn activate_workspace_root(
        &mut self,
        id: crate::workspace::RootId,
    ) {
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                self.clear_git_status();
                let path = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.display().to_string())
                    .unwrap_or_default();
                self.ui.notification =
                    Some(self.message("workspace_root_activated", &[("path", path)]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }
}
