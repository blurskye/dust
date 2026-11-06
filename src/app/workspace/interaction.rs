use crate::app::App;
use crate::app::TrustedAction;
use crate::app::tasks::AdapterContext;
use crate::app::workspace::WorkspaceFailure;
use crate::app::workspace::WorkspacePreflightEvent as ExecutionPreflightEvent;
use crate::app::workspace::WorkspaceUpdate;
use crate::app::workspace::leaves_root;
use crate::tools::RegistryRequest;
use crate::tools::ToolStatus;
use crate::ui::PromptKind;
use crate::workspace::DiagnosticOwner;
use crate::workspace::ExecutionSource;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

impl App {
    pub(in crate::app) fn pending_preflight_count(&self) -> usize {
        self.tasks.preflight_count()
            + self.terminal_session.preflight_count()
            + self.language.preflight_count()
    }
}

impl App {
    pub(in crate::app) fn process_execution_preflight(&mut self) {
        let events = self.workspace.preflight_events();
        for event in events {
            match event {
                ExecutionPreflightEvent::Ready(ready) => {
                    if self
                        .debugger_session
                        .owns_debug_preflight(ready.result().id)
                    {
                        let id = ready.result().id;
                        if let Err(error) = self.debugger_session.debug_preflight(ready) {
                            self.debugger_session.fail_debug_preflight(id);
                            self.show_error(error);
                        }
                        self.advance_debug_launch();
                        continue;
                    }
                    if self.language.owns_preflight(ready.result().id) {
                        match self.language.finish_preflight(
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                            &self.configuration.settings().lsp,
                        ) {
                            Ok(Some(key)) => {
                                for prefix in ["lsp.", "lsp-pull:"] {
                                    if let Some(producer) =
                                        self.language.diagnostic_producer(&key, prefix)
                                    {
                                        self.diagnostics.activate(producer);
                                    }
                                }
                                self.sync_ui_diagnostics();
                            }
                            Ok(None) => {}
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    let result = ready.result();
                    let id = result.id;
                    if result.root_revision != self.workspace.root_revision()
                        || self
                            .workspace
                            .authorize_execution(ExecutionSource::ProjectLocal)
                            .is_err()
                    {
                        if let Some(step) = self.tasks.take_preflight(id) {
                            self.tasks.reject_step(&step);
                        }
                        if let Some(key) = self.terminal_session.take_preflight(id) {
                            if let Some(id) = self.terminal_session.resolve(&key)
                                && let Some(reply) = self.debugger_session.take_terminal(&id)
                            {
                                let _ =
                                    reply.respond(false, Some("terminal workspace retired".into()));
                            }
                            self.terminal_session.reject(&key);
                            self.sync_terminal_panel();
                        }
                        continue;
                    }
                    if let Some(step) = self.tasks.take_preflight(id) {
                        match self.tasks.launch(
                            step,
                            ready,
                            &self.state,
                            &self.workspace,
                            self.configuration.binding_revision(),
                        ) {
                            Ok(source) => {
                                if let Some(source) = source {
                                    self.diagnostics.activate(source);
                                    self.sync_ui_diagnostics();
                                }
                            }
                            Err(error) => self.show_error(error),
                        }
                        continue;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        let id = self.terminal_session.resolve(&key);
                        if id.is_some_and(|id| !self.debugger_session.terminal_current(&id)) {
                            self.terminal_session.reject(&key);
                            if let Some(id) = id {
                                self.debugger_session.take_terminal(&id);
                            }
                            continue;
                        }
                        if let Err(error) =
                            self.terminal_session
                                .launch(key.clone(), ready, &self.workspace)
                        {
                            self.terminal_session.reject(&key);
                            if let Some(reply) =
                                id.and_then(|id| self.debugger_session.take_terminal(&id))
                            {
                                let _ = reply.respond(false, Some(error.clone()));
                            }
                            self.show_error(error);
                        }
                        self.sync_terminal_panel();
                        continue;
                    }
                }
                ExecutionPreflightEvent::Failed {
                    id,
                    root_revision,
                    message,
                } => {
                    if self.debugger_session.owns_debug_preflight(id) {
                        self.debugger_session.fail_debug_preflight(id);
                        self.show_error(message);
                        continue;
                    }
                    if self.language.fail_preflight(id) {
                        self.show_error(message);
                        continue;
                    }
                    let mut owned = false;
                    if let Some(step) = self.tasks.take_preflight(id) {
                        self.tasks.reject_step(&step);
                        owned = true;
                    }
                    if let Some(key) = self.terminal_session.take_preflight(id) {
                        if let Some(id) = self.terminal_session.resolve(&key)
                            && let Some(reply) = self.debugger_session.take_terminal(&id)
                        {
                            let _ = reply.respond(false, Some(message.clone()));
                        }
                        self.terminal_session.reject(&key);
                        self.sync_terminal_panel();
                        owned = true;
                    }
                    if owned && root_revision == self.workspace.root_revision() {
                        self.show_error(message)
                    }
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn add_workspace_root(&mut self, path: PathBuf) {
        let requested_path = if path.is_absolute() {
            path
        } else {
            self.state.workspace_root.join(path)
        };
        match self.workspace.add_root_request(requested_path, None) {
            Ok(()) => self.notify_message(self.message("workspace_root_loading", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn switch_workspace_root(&mut self, path: PathBuf) {
        let current = self.workspace.active_root().and_then(|id| {
            self.workspace
                .roots()
                .iter()
                .find(|root| root.id == id)
                .map(|root| (id, root.identity.canonical_path.clone()))
        });
        let Some((id, current_path)) = current else {
            self.add_workspace_root(path);
            return;
        };
        let target = path.canonicalize().unwrap_or_else(|_| path.clone());
        if target == current_path {
            self.close_picker();
            self.notify_message(format!("already in {}", target.display()), false);
            return;
        }
        // A folder that is already a workspace root is made active, not opened in place of this one.
        if let Some(existing) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.identity.canonical_path == target)
            .map(|root| root.id)
        {
            self.close_picker();
            self.activate_workspace_root(existing);
            return;
        }
        let unsaved = self
            .state
            .buffers
            .values()
            .filter(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_ref()
                        .is_some_and(|file| leaves_root(file, &current_path, &target))
            })
            .count();
        if unsaved > 0 {
            self.show_error(format!(
                "{unsaved} file(s) in {} have unsaved changes; save or close them first",
                current_path.file_name().map_or_else(
                    || current_path.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ));
            return;
        }
        // Terminals belong to the folder they were opened in, so switching ends them, as
        // opening another folder does in VS Code; it takes asking twice when any are open.
        let terminals = self.terminal_session.running_in_root(&current_path);
        if terminals > 0 && self.switch_ends_terminals.as_ref() != Some(&target) {
            self.switch_ends_terminals = Some(target);
            self.show_error(format!(
                "switching ends {terminals} open terminal(s) and what runs in them; switch again \
                 to go ahead"
            ));
            return;
        }
        self.switch_ends_terminals = None;
        self.close_picker();
        match self.workspace.add_root_request(target, Some(id)) {
            Ok(()) => {
                self.switch_approved_terminals = self.terminal_session.ids_in_root(&current_path);
                self.notify_message("switching…".to_owned(), false);
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    /// After a worktree switch: close the old root's clean buffers, move the editor's root and
    /// reopen the file that was in front at the same place in the new worktree.
    pub(in crate::app) fn finish_root_switch(&mut self, path: PathBuf, old: &Path) {
        self.terminal_session.close_root(old);
        let leaving = |file: &Path| leaves_root(file, old, &path);
        let reopen = self.current_persisted_location().and_then(|mut location| {
            let relative = location.path.strip_prefix(old).ok()?;
            if !leaving(&location.path) {
                return None;
            }
            location.path = path.join(relative);
            location.path.is_file().then_some(location)
        });
        // Buffers in no window go first, or closing a window's buffer would put one of them on
        // screen, and a later save would land in the old checkout.
        let hidden = self
            .state
            .buffers
            .iter()
            .filter(|(id, buffer)| {
                !buffer.dirty
                    && buffer.path.as_deref().is_some_and(leaving)
                    && !self.state.views.values().any(|view| view.buffer == *id)
            })
            .map(|(id, buffer)| (id, buffer.path.clone()))
            .collect::<Vec<_>>();
        for (buffer, closed_path) in hidden {
            let mut candidate = self.state.draft();
            candidate.remove_hidden_document(buffer);
            match self.commit_editor_draft(candidate, None, false) {
                Ok(()) => self.retire_closed_buffer(buffer, closed_path.as_deref(), true),
                Err(error) => self.show_error(error),
            }
        }
        // Finish one buffer before choosing the next: otherwise fallback buffers can alternate
        // forever when several panes show the same leaving buffer.
        let leaving_buffers = self
            .state
            .buffers
            .iter()
            .filter(|(_, buffer)| !buffer.dirty && buffer.path.as_deref().is_some_and(leaving))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for buffer in leaving_buffers {
            let views = self
                .state
                .views
                .iter()
                .filter(|(_, state)| state.buffer == buffer)
                .map(|(view, _)| view)
                .collect::<Vec<_>>();
            for view in views {
                if let Err(error) = self.state.focus_view(view) {
                    self.show_error(error);
                    break;
                }
                self.close_buffer_target(view, buffer);
            }
        }
        // Windows left with nothing to show go too, down to the one the file reopens in.
        let front = self.state.active_view;
        let empty = self
            .state
            .views
            .iter()
            .filter(|(view, state)| {
                Some(*view) != front
                    && self.state.buffers.get(state.buffer).is_some_and(|buffer| {
                        buffer.path.is_none() && !buffer.dirty && buffer.text.len_bytes() == 0
                    })
                    && Some(state.buffer) != self.state.kept_buffer
            })
            .map(|(view, state)| (view, state.buffer))
            .collect::<Vec<_>>();
        for (view, buffer) in empty {
            if self.state.close_view(view).is_ok() {
                let mut candidate = self.state.draft();
                candidate.remove_hidden_document(buffer);
                if let Err(error) = self.commit_editor_draft(candidate, None, false) {
                    self.show_error(error);
                }
            }
        }
        if let Some(front) = front {
            let _ = self.state.focus_view(front);
        }
        self.state.set_workspace_root(path.clone());
        self.invalidate_root_scoped_operations();
        self.start_git_status_refresh_silent();
        if let Some(location) = reopen {
            self.navigate_persisted_location(location);
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.notify_message(format!("switched to {name}"), false);
    }
}

impl App {
    pub(in crate::app) fn root_removal_blocker(
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
        if self
            .terminal_session
            .belongs_to_root(&root.identity.canonical_path)
        {
            return Some("Close the root’s terminals before removing it".into());
        }
        let busy = self.workspace.root_operation_pending()
            || self.language.active().is_some()
            || self
                .language
                .sessions()
                .values()
                .any(|state| state.restart_due.is_some())
            || self.debugger_session.is_running()
            || self
                .tooling
                .entries()
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Installing { .. }))
            || self.search.pending()
            || self.workspace_edits.preparing()
            || self.workspace_edits.proposal().is_some()
            || self.documents.legacy_pending()
            || self.workspace.persist_pending()
            || self.documents.has_startup_work()
            || self.documents.file_work_pending();
        busy.then(|| self.message("workspace_root_service_blocker", &[]))
    }
}

impl App {
    pub(in crate::app) fn invalidate_root_scoped_operations(&mut self) {
        self.documents.retire_startup();
        self.documents.retire_workspace(&self.workspace.version());
        self.clear_git_status();
        self.terminal_session.retain_workspace(&self.workspace);
        self.reject_terminal_responses("workspace changed");
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.id)
            .collect::<HashSet<_>>();
        let stale = self
            .language
            .sessions()
            .iter()
            .filter_map(|(key, state)| (!roots.contains(&state.root)).then_some(key.clone()))
            .collect::<Vec<_>>();
        let removed_lsp = !stale.is_empty();
        for key in stale {
            let _ = self.language.stop(&key);
            self.clear_lsp_progress(&key);
            self.clear_lsp_server_ownership(&key);
            self.language.remove(&key);
            self.language.forget_synced(&key);
            if self.language.active().map(String::as_str) == Some(&key) {
                self.language.clear_active();

                self.clear_lsp_ui_projection();
            }
        }
        if removed_lsp {
            self.sync_ui_diagnostics();
        }
        self.language.retire_launches();
        for (plugin, load) in self.extension.retire_packages() {
            self.remove_plugin_contribution_generation(&plugin, load);
        }
        self.debugger_session.reset();
        self.input.clear_snippet();
        for name in self.tasks.names() {
            if let Ok(owner) = DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.tasks.retire_workspace();
        self.clear_git_status();
        self.search.cancel();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.retire_workspace_resources("workspace changed");
        self.retire_plugin_edit_sources();
        self.retire_explorer_intents();
        self.explorer
            .reset_roots(self.workspace.roots(), self.workspace.root_revision());
        self.ui.explorer = None;
        self.set_explorer_focus(false);
        self.reconcile_explorer_watch();
        self.close_picker();
        self.ensure_cursor_visible();
    }
}

impl App {
    pub(in crate::app) fn remove_workspace_root(&mut self, id: crate::workspace::RootId) {
        if let Some(blocker) = self.root_removal_blocker(id) {
            self.show_error(blocker);
            return;
        }
        match self.workspace.remove_root(id) {
            Ok(root) => {
                if let Some(active) = self
                    .workspace
                    .active_root()
                    .and_then(|id| self.workspace.roots().iter().find(|root| root.id == id))
                {
                    self.state
                        .set_workspace_root(active.identity.canonical_path.clone());
                }
                self.invalidate_root_scoped_operations();
                self.notify_message(
                    self.message(
                        "workspace_root_removed",
                        &[("path", root.identity.canonical_path.display().to_string())],
                    ),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn activate_workspace_root(&mut self, id: crate::workspace::RootId) {
        let before = self.workspace.version();
        match self.workspace.set_active_root(id) {
            Ok(()) => {
                if before != self.workspace.version() {
                    self.documents.retire_startup();
                }
                self.documents.retire_workspace(&self.workspace.version());
                self.clear_git_status();
                let root = self
                    .workspace
                    .roots()
                    .iter()
                    .find(|root| root.id == id)
                    .map(|root| root.identity.canonical_path.clone());
                // The statusline and relative labels name the active root.
                if let Some(root) = &root {
                    self.state.set_workspace_root(root.clone());
                }
                let path = root
                    .map(|root| root.display().to_string())
                    .unwrap_or_default();
                self.notify_message(
                    self.message("workspace_root_activated", &[("path", path)]),
                    false,
                );
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn show_workspace_failure(&mut self, failure: WorkspaceFailure) {
        let message = match failure {
            WorkspaceFailure::Message(key) => self.message(key, &[]),
            WorkspaceFailure::Diagnostic(key, diagnostic) => {
                self.message(key, &[("diagnostic", diagnostic)])
            }
            WorkspaceFailure::Plain(message) => message,
        };
        self.show_error(message);
    }
}

impl App {
    /// Run a project-defined command if the workspace is trusted.
    pub(in crate::app) fn run_trusted(&mut self, action: TrustedAction) {
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        match action {
            TrustedAction::StartDebug(intent) => self.authorize_debug_launch(intent),
            TrustedAction::InstallRegistry {
                publisher,
                name,
                version,
            } => self.request_registry(
                RegistryRequest::Show {
                    publisher,
                    name,
                    version,
                },
                true,
            ),
            TrustedAction::PrepareRegistryArtifact => {
                if let Err(error) = self.extension.prepare_artifact(self.plugin_limits()) {
                    self.show_error(error);
                }
            }
            TrustedAction::InstallPackage {
                package_manifest,
                plugin_manifest,
                component,
                update,
            } => self.install_package_after_validation(
                package_manifest,
                plugin_manifest,
                component,
                update,
            ),
            // A task with a matcher reports what it matched in Problems, as a VS Code task does.
            TrustedAction::RunTask(name) => {
                let adapter = match self.configuration.settings().tasks.get(&name) {
                    Some(task) => task
                        .matcher
                        .clone()
                        .map_or(AdapterContext::Plain, |matcher| AdapterContext::Linter {
                            matcher,
                            lint: task.kind == crate::config::TaskKind::Linter,
                        }),
                    None => AdapterContext::Plain,
                };
                self.start_task_plan(&name, adapter)
            }
            TrustedAction::OpenTerminal(intent) => self.open_terminal_with_response(intent, None),
            TrustedAction::OpenDapTerminal { intent, reply } => {
                self.open_terminal_with_response(intent, Some(reply))
            }
            TrustedAction::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            TrustedAction::RunLinter { name, matcher } => self.start_task_plan(
                &name,
                AdapterContext::Linter {
                    matcher,
                    lint: true,
                },
            ),
            TrustedAction::RunFormatter { name, source } => {
                self.start_task_plan(&name, AdapterContext::Formatter { source })
            }
        }
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn reconcile_workspace_trust(&mut self) {
        self.workspace.reconcile_trust();
    }
}

impl App {
    pub(in crate::app) fn process_trust_events(&mut self) {
        if let Some((old, new)) = self.workspace.pending_root_switch() {
            let dirty = self.state.buffers.values().any(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_deref()
                        .is_some_and(|file| leaves_root(file, old, new))
            });
            let new_terminal = self
                .terminal_session
                .ids_in_root(old)
                .iter()
                .any(|id| !self.switch_approved_terminals.contains(id));
            if dirty || new_terminal {
                self.workspace.cancel_root_switch();
                self.switch_approved_terminals.clear();
                self.show_error(if dirty {
                    "Workspace switch cancelled: save or close the unsaved files first"
                } else {
                    "Workspace switch cancelled: new terminals opened; switch again to confirm"
                });
            }
        }
        let before = self.workspace.version();
        let updates = self.workspace.poll(&self.state);
        if before != self.workspace.version() {
            self.documents.retire_workspace(&self.workspace.version());
        }
        for event in updates {
            match event {
                WorkspaceUpdate::Failure(e) => self.show_workspace_failure(e),
                WorkspaceUpdate::Message(key) => self.notify_message(self.message(key, &[]), false),
                WorkspaceUpdate::RootAdded(path) => {
                    self.invalidate_root_scoped_operations();
                    // A new root becomes the active one; the statusline and relative paths follow.
                    self.clear_git_status();
                    self.state.set_workspace_root(path.clone());
                    self.notify_message(
                        self.message(
                            "workspace_root_added",
                            &[("path", path.display().to_string())],
                        ),
                        false,
                    );
                }
                WorkspaceUpdate::RootSwitched { path, old } => self.finish_root_switch(path, &old),
            }
        }
    }
}

impl App {
    pub(in crate::app) fn maybe_prompt_for_unseen_workspace(&mut self) {
        // Never open under the user's fingers: keys typed a moment ago would answer it.
        let typing = self
            .last_key_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(800));
        if self.workspace.should_prompt(
            typing
                || self.input.prompt().is_some()
                || self.ui.picker.is_some()
                || self.ui.tool_menu.is_some()
                || self.ui.settings.is_some(),
        ) {
            self.request_workspace_trust();
        }
    }
}

impl App {
    pub(in crate::app) fn request_workspace_trust(&mut self) {
        if self.input.prompt().is_some() {
            return;
        }
        match self.workspace.request_confirmation() {
            Ok(()) => {
                self.clear_notice();
                self.open_confirmation_actions(
                    PromptKind::WorkspaceTrust,
                    self.configuration
                        .settings()
                        .ui
                        .command_line
                        .text
                        .workspace_trust_title
                        .clone(),
                    self.message("workspace_trust_confirmation", &[]),
                    crate::ui::ConfirmationChoice::Yes,
                    "Trust",
                    "Not now",
                );
            }
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn confirm_workspace_trust(&mut self) {
        match self.workspace.confirm() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn revoke_workspace_trust(&mut self) {
        match self.workspace.revoke() {
            Ok(()) => self.notify_message(self.message("workspace_trust_saving", &[]), false),
            Err(e) => self.show_workspace_failure(e),
        }
    }
}

impl App {
    pub(in crate::app) fn workspace_relative_label(&self, path: &std::path::Path) -> String {
        self.workspace
            .roots()
            .iter()
            .find_map(|root| path.strip_prefix(&root.identity.canonical_path).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }
}
