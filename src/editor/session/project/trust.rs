use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn update_live_crash_snapshot(&mut self) {
        let now = Instant::now();
        if now < self.next_crash_snapshot {
            return;
        }
        self.next_crash_snapshot = now + Duration::from_secs(1);
        let timestamp_ms = crate::clock::system_time()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let _ = self.crash_snapshot.submit(LiveCrashSnapshot {
            timestamp_ms,
            roots: self.workspace.roots().len(),
            buffers: self.state.buffers.len(),
            dirty_buffers: self
                .state
                .buffers
                .values()
                .filter(|buffer| buffer.dirty)
                .count(),
            jobs: self.job_tasks.len(),
            ptys: self.terminals.len(),
            lsp_sessions: self.lsp_sessions.len(),
            dap_running: self.dap.is_running(),
            pending_effects: self
                .pending_vfs
                .len()
                .saturating_add(self.pending_execution_preflight.len())
                .saturating_add(self.pending_linter_builds.len()),
            plugins_failed: self
                .plugins
                .statuses
                .values()
                .filter(|status| matches!(status, PluginStatus::Failed(_)))
                .count(),
            safe_mode: self.safe_mode,
        });
    }

    pub(in crate::editor::session) fn defer_trusted_execution(
        &mut self,
        action: DeferredExecution,
    ) {
        if self.safe_mode {
            self.show_error(self.message("safe_mode_blocked", &[]));
            return;
        }
        if self.execution_validation.is_some() {
            self.show_error(self.message("workspace_execution_validation_busy", &[]));
            return;
        }
        if let Err(error) = self
            .workspace
            .authorize_execution(ExecutionSource::ProjectLocal)
        {
            self.show_error(error);
            return;
        }
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        let roots = self.workspace.roots().to_vec();
        let mut generations = Vec::new();
        for root in &roots {
            match self
                .trust_service
                .inspect_root(root.identity.canonical_path.clone())
            {
                Ok(generation) => generations.push((generation, root.id, root.identity.clone())),
                Err(error) => {
                    self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    self.show_error(self.message(
                        "workspace_execution_validation_failed",
                        &[("diagnostic", error.to_string())],
                    ));
                    return;
                }
            }
        }
        self.execution_validation_generations.clear();
        for (generation, root, identity) in generations {
            self.execution_validation_generations
                .insert(generation, (root, identity));
        }
        self.execution_validation = Some(PendingExecutionValidation {
            action,
            root_revision: self.workspace.root_revision(),
            failed: false,
        });
        self.ui.notification = Some(self.message("workspace_execution_validating", &[]));
        self.ui.notification_error = false;
    }

    pub(in crate::editor::session) fn finish_execution_validation_if_ready(&mut self) {
        if !self.execution_validation_generations.is_empty() {
            return;
        }
        let Some(pending) = self.execution_validation.take() else {
            return;
        };
        if pending.failed || pending.root_revision != self.workspace.root_revision() {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message(
                "workspace_execution_validation_failed",
                &[("diagnostic", "workspace root identity changed".into())],
            ));
            return;
        }
        let validating = self.message("workspace_execution_validating", &[]);
        if self.ui.notification.as_deref() == Some(validating.as_str()) {
            self.ui.notification = None;
            self.ui.notification_error = false;
        }
        self.execution_revalidated = true;
        match pending.action {
            DeferredExecution::InstallTool { id, source } => self.install_tool_by_id(&id, source),
            DeferredExecution::StartLsp(requested) => self.start_lsp(requested),
            DeferredExecution::RestartLsp(key) => self.restart_lsp_session(key),
            DeferredExecution::StartDap { requested, program } => {
                self.start_dap(requested, program);
            }
            DeferredExecution::StartDapConfiguration(name) => self.start_dap_configuration(&name),
            DeferredExecution::InstallRegistry {
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
            DeferredExecution::PrepareRegistryArtifact => {
                if let Some((metadata, component)) = self.pending_registry_artifact.take()
                    && let Err(error) = self.packages.request(PackageOperation::PrepareRegistry {
                        metadata: Box::new(metadata),
                        component,
                        limits: self.plugin_limits(),
                    })
                {
                    self.show_error(error)
                }
            }
            DeferredExecution::InstallPackage {
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
            DeferredExecution::RunTask(name) => self.start_task_after_validation(&name),
            DeferredExecution::OpenTerminal(profile) => {
                self.open_terminal_after_validation_with_response(&profile, None)
            }
            DeferredExecution::OpenDapTerminal {
                profile,
                request_seq,
            } => self.open_terminal_after_validation_with_response(&profile, Some(request_seq)),
            DeferredExecution::RunTest { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Test { matcher })
            }
            DeferredExecution::RunLinter { name, matcher } => {
                self.start_task_plan(&name, AdapterContext::Linter { matcher })
            }
            DeferredExecution::RunFormatter {
                name,
                buffer,
                revision,
                source,
            } => self.start_task_plan(
                &name,
                AdapterContext::Formatter {
                    buffer,
                    revision,
                    source,
                },
            ),
        }
        self.execution_revalidated = false;
    }

    pub(in crate::editor::session) fn reconcile_workspace_trust(&mut self) {
        let trusted = self.trust_database_ready
            && self.trust_identity_ready
            && self.workspace.roots().iter().all(|root| {
                root.identity.filesystem_identity.is_some()
                    && self.trust_store.is_trusted(&root.identity)
            });
        self.workspace.set_trust(if trusted {
            WorkspaceTrust::Trusted
        } else {
            WorkspaceTrust::Untrusted
        });
    }

    pub(in crate::editor::session) fn process_trust_events(&mut self) {
        loop {
            let event = match self.trust_service.try_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    self.execution_validation = None;
                    self.execution_validation_generations.clear();
                    self.show_error(self.message(
                        "workspace_trust_state_failed",
                        &[("diagnostic", error.to_string())],
                    ));
                    break;
                }
            };
            match event {
                TrustEvent::Loaded { generation, result }
                    if self.trust_load_generation == Some(generation) =>
                {
                    self.trust_load_generation = None;
                    self.trust_database_ready = true;
                    match result {
                        Ok(database) => {
                            self.trust_store.replace(database.identities);
                            self.trust_store.replace_seen(database.seen);
                        }
                        Err(error) => {
                            self.trust_store.replace(Vec::new());
                            self.trust_store.replace_seen(Vec::new());
                            self.show_error(self.message(
                                "workspace_trust_state_failed",
                                &[("diagnostic", error.to_string())],
                            ));
                        }
                    }
                    self.reconcile_workspace_trust();
                }
                TrustEvent::RootInspected { generation, result }
                    if self.trust_identity_generation == Some(generation) =>
                {
                    self.trust_identity_generation = None;
                    match result {
                        Ok(identity) => {
                            let root = self.workspace.active_root();
                            if let Some(root) = root {
                                match self.workspace.replace_root_identity(root, identity) {
                                    Ok(()) => self.trust_identity_ready = true,
                                    Err(error) => self.show_error(error),
                                }
                            }
                        }
                        Err(error) => self.show_error(error),
                    }
                    self.reconcile_workspace_trust();
                }
                TrustEvent::RootInspected { generation, result }
                    if self
                        .pending_root_operation
                        .as_ref()
                        .is_some_and(|operation| operation.generation == generation) =>
                {
                    let operation = self
                        .pending_root_operation
                        .take()
                        .expect("matched root operation");
                    if operation.root_revision != self.workspace.root_revision() {
                        self.show_error(self.message("workspace_root_stale", &[]));
                        continue;
                    }
                    match result {
                        Ok(identity) => {
                            let name = identity.canonical_path.file_name().map_or_else(
                                || identity.canonical_path.display().to_string(),
                                |name| name.to_string_lossy().into_owned(),
                            );
                            match self.workspace.add_root(name, identity.clone()) {
                                Ok(_) => {
                                    self.invalidate_root_scoped_operations();
                                    self.ui.notification = Some(self.message(
                                        "workspace_root_added",
                                        &[("path", identity.canonical_path.display().to_string())],
                                    ));
                                    self.ui.notification_error = false;
                                }
                                Err(error) => self.show_error(error),
                            }
                        }
                        Err(error) => self.show_error(self.message(
                            "workspace_root_add_failed",
                            &[
                                ("path", operation.requested_path.display().to_string()),
                                ("diagnostic", error.to_string()),
                            ],
                        )),
                    }
                }
                TrustEvent::RootInspected { generation, result }
                    if self
                        .execution_validation_generations
                        .contains_key(&generation) =>
                {
                    let (_, expected) = self
                        .execution_validation_generations
                        .remove(&generation)
                        .expect("checked execution validation generation");
                    let valid = result.as_ref().is_ok_and(|identity| identity == &expected)
                        && self.trust_store.is_trusted(&expected);
                    if !valid {
                        if let Some(pending) = &mut self.execution_validation {
                            pending.failed = true;
                        }
                        self.workspace.set_trust(WorkspaceTrust::Untrusted);
                    }
                    self.finish_execution_validation_if_ready();
                }
                TrustEvent::Persisted { generation, result }
                    if self.trust_persist_generation == Some(generation) =>
                {
                    self.trust_persist_generation = None;
                    let target = self.trust_persist_target.take();
                    match result {
                        Ok(()) => {
                            self.reconcile_workspace_trust();
                            if let Some(target) = target {
                                let key = if target == WorkspaceTrust::Trusted {
                                    "workspace_trusted"
                                } else {
                                    "workspace_untrusted"
                                };
                                self.ui.notification = Some(self.message(key, &[]));
                                self.ui.notification_error = false;
                            }
                        }
                        Err(error) => {
                            if target == Some(WorkspaceTrust::Trusted) {
                                for root in self.workspace.roots() {
                                    self.trust_store.revoke(&root.identity);
                                }
                            }
                            self.workspace.set_trust(WorkspaceTrust::Untrusted);
                            self.show_error(self.message(
                                "workspace_trust_state_failed",
                                &[("diagnostic", error.to_string())],
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(in crate::editor::session) fn persist_workspace_trust(&mut self, target: WorkspaceTrust) {
        match self.trust_service.persist(
            self.trust_state_path.clone(),
            self.trust_store.identities(),
            self.trust_store.seen_identities(),
        ) {
            Ok(generation) => {
                self.trust_persist_generation = Some(generation);
                self.trust_persist_target = Some(target);
                self.ui.notification = Some(self.message("workspace_trust_saving", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => {
                self.workspace.set_trust(WorkspaceTrust::Untrusted);
                self.show_error(self.message(
                    "workspace_trust_state_failed",
                    &[("diagnostic", error.to_string())],
                ));
            }
        }
    }

    pub(in crate::editor::session) fn persist_seen_workspaces(&mut self) {
        match self.trust_service.persist(
            self.trust_state_path.clone(),
            self.trust_store.identities(),
            self.trust_store.seen_identities(),
        ) {
            Ok(generation) => {
                self.trust_persist_generation = Some(generation);
                self.trust_persist_target = None;
            }
            Err(error) => self.show_error(self.message(
                "workspace_trust_state_failed",
                &[("diagnostic", error.to_string())],
            )),
        }
    }

    pub(in crate::editor::session) fn maybe_prompt_for_unseen_workspace(&mut self) {
        if self.startup_trust_prompt_checked
            || !self.trust_database_ready
            || !self.trust_identity_ready
            || self.ui.prompt.is_some()
            || self.ui.picker.is_some()
            || self.ui.tool_menu.is_some()
        {
            return;
        }
        self.startup_trust_prompt_checked = true;
        if self.safe_mode || self.workspace.trust() == WorkspaceTrust::Trusted {
            return;
        }
        let Some(active_root) = self.workspace.active_root() else {
            return;
        };
        let Some(identity) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == active_root)
            .map(|root| &root.identity)
        else {
            return;
        };
        if self.trust_store.is_seen(identity) {
            return;
        }
        self.request_workspace_trust();
    }

    pub(in crate::editor::session) fn request_workspace_trust(&mut self) {
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        self.open_prompt(PromptKind::WorkspaceTrust, String::new());
        self.ui.notification = None;
        self.ui.notification_error = false;
        let identities = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for identity in identities {
            changed |= self.trust_store.mark_seen(identity);
        }
        if changed {
            self.persist_seen_workspaces();
        }
        let confirmation = self.message(
            "workspace_trust_confirmation",
            &[(
                "phrase",
                self.configuration
                    .settings
                    .workspace
                    .trust_confirmation_phrase
                    .clone(),
            )],
        );
        if let Some(prompt) = &mut self.ui.prompt {
            prompt.help = Some(confirmation);
        }
    }

    pub(in crate::editor::session) fn confirm_workspace_trust(&mut self, phrase: String) {
        if !self.trust_database_ready || !self.trust_identity_ready {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
            return;
        }
        if phrase
            != self
                .configuration
                .settings
                .workspace
                .trust_confirmation_phrase
        {
            self.workspace.set_trust(WorkspaceTrust::Untrusted);
            self.show_error(self.message("workspace_trust_confirmation_wrong", &[]));
            return;
        }
        for root in self.workspace.roots() {
            if root.identity.filesystem_identity.is_none() {
                self.show_error(self.message("workspace_trust_not_ready", &[]));
                return;
            }
            self.trust_store.trust(root.identity.clone());
        }
        self.persist_workspace_trust(WorkspaceTrust::Trusted);
    }

    pub(in crate::editor::session) fn revoke_workspace_trust(&mut self) {
        self.workspace.set_trust(WorkspaceTrust::Untrusted);
        for root in self.workspace.roots() {
            self.trust_store.revoke(&root.identity);
        }
        if self.trust_database_ready {
            self.persist_workspace_trust(WorkspaceTrust::Untrusted);
        } else {
            self.show_error(self.message("workspace_trust_not_ready", &[]));
        }
    }
}
