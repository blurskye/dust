use crate::app::App;
use crate::app::documents::mode_name_large;
use crate::app::persistence::RECENT_FILE_LIMIT;
use crate::app::persistence::RestoreContext;
use crate::app::persistence::RestorePrepared;
use crate::app::persistence::RestoreReady;
use crate::app::persistence::STATE_ENTRY_LIMIT;
use crate::app::persistence::STATE_MAXIMUM_BYTES;
use crate::app::windows::NativeDescriptions;
use crate::app::windows::RestoreWindowPolicy;
use crate::app::windows::WindowCandidate;
use crate::persistence::PersistedLargeFileOverride;
use crate::persistence::PersistedSession;
use crate::persistence::PersistedState;
use crate::persistence::PersistedWindowRule;
use crate::persistence::PersistencePolicy;
use crate::persistence::STATE_VERSION;
use crate::ui::PromptKind;
use crate::ui::render::SCRATCH_LABEL;
use std::path::PathBuf;

impl App {
    pub(super) fn persistence_policy(&self) -> PersistencePolicy {
        PersistencePolicy {
            maximum_file_bytes: STATE_MAXIMUM_BYTES,
            maximum_entries: STATE_ENTRY_LIMIT,
        }
    }
}

impl App {
    /// Asked for by the user, a saved session replaces what is open unless that would lose work.
    pub(in crate::app) fn load_session_on_request(&mut self) {
        if !self.configuration.settings().persistence.enabled {
            self.show_error(self.message("session_disabled", &[]));
            return;
        }
        if self.state.buffers.values().any(|buffer| buffer.dirty) {
            self.show_error(self.message("session_load_dirty", &[]));
            return;
        }
        let result = self.persistence.load_session(
            self.persistence_policy(),
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            true,
        );
        match result {
            Ok(()) => {
                self.session_load_requested = true;
                self.tooling.begin_restore();
                self.debugger_session.begin_restore();
                self.configuration.begin_theme_restore();
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn load_persistence(&mut self) {
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled || !configured.restore_on_startup {
            return;
        }
        if let Err(error) = self.persistence.load_session(
            self.persistence_policy(),
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            !self.documents.has_startup_work() && self.state.buffers.is_empty(),
        ) {
            self.ui.config_diagnostic = Some(error);
        } else {
            self.tooling.begin_restore();
            self.debugger_session.begin_restore();
            self.configuration.begin_theme_restore();
        }
    }
}

impl App {
    pub(in crate::app) fn capture_window_rules(&self) -> Vec<PersistedWindowRule> {
        self.window_session
            .persisted_rules(self.terminal_session.selected().map(|id| id.0))
    }
}

impl App {
    pub(in crate::app) fn prepare_configured_window_rules(
        &self,
        windows: &crate::config::WindowsConfig,
    ) -> Result<WindowCandidate, String> {
        let terminals = self.terminal_session.descriptions();
        self.window_session.prepare_rules(
            windows,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )
    }
}

impl App {
    pub(in crate::app) fn apply_persisted_window_rules(
        &mut self,
        rules: &[PersistedWindowRule],
    ) -> Result<(), String> {
        let terminals = self.terminal_session.descriptions();
        let candidate = self.window_session.prepare_persisted_rules(
            rules,
            NativeDescriptions {
                signature: self.native_surface_signature(self.window_session.editor_ids()),
                explorer_title: &self.message("explorer_title", &[]),
                dashboard_title: &self.configuration.dashboard().identity,
                picker_title: self
                    .ui
                    .picker
                    .as_ref()
                    .map_or("Picker", |picker| &picker.title),
                terminals: &terminals,
            },
        )?;
        self.window_session
            .accept_current(candidate, &self.state.window_version())?;
        let requires_projection = self.window_session.graph().active_tab().is_some();
        self.reconcile_editor_windows();
        if requires_projection && self.compositor_projection().is_none() {
            return Err("persisted window projection could not be published".into());
        }
        Ok(())
    }
}

impl App {
    #[cfg(test)]
    pub(in crate::app) fn capture_session(&self) -> Option<PersistedSession> {
        self.try_capture_session().ok().flatten()
    }
}

impl App {
    pub(in crate::app) fn try_capture_session(&self) -> Result<Option<PersistedSession>, String> {
        self.persistence.capture_session(
            &self.state,
            &self.workspace,
            &self.window_session,
            self.capture_window_rules(),
            &self.extension.virtual_descriptor_sources(),
        )
    }
}

impl App {
    pub(in crate::app) fn save_persistence(&mut self) {
        self.write_persistence();
    }
}

impl App {
    pub(in crate::app) fn write_persistence(&mut self) {
        self.persistence.reconcile_marks(&self.state);
        let configured = &self.configuration.settings().persistence;
        if !configured.enabled {
            return;
        }
        if self.persistence.pending() {
            self.persistence.request_save();
            return;
        }
        let mut metadata = self.tooling.metadata();
        match self.configuration.theme_state() {
            Ok(Some(theme)) => {
                metadata.insert("ui.theme.snapshot".into(), theme);
            }
            Ok(None) => {}
            Err(error) => {
                self.ui.config_diagnostic = Some(error);
                return;
            }
        }
        // During the tutor the layout to keep is the user's own, not the practice project's.
        let captured = match self.tutor.saved_layout() {
            Some(layout) => Ok(layout.clone()),
            None => self.try_capture_session(),
        };
        let session = match captured {
            Ok(session) => session,
            Err(error) => {
                self.ui.config_diagnostic = Some(error);
                return;
            }
        };
        let state = PersistedState {
            version: STATE_VERSION,
            session,
            recent_files: self.persistence.recent_files().to_vec(),
            marks: self.persistence.marks().to_vec(),
            jumps: self.persistence.jumps().to_vec(),
            changes: self.persistence.changes().to_vec(),
            command_history: self.input.command_history().to_vec(),
            search_history: self.input.search_history().to_vec(),
            breakpoints: self.debugger_session.persisted_breakpoints(),
            large_file_overrides: self
                .documents
                .policy_overrides()
                .filter_map(|(id, mode)| {
                    let path = self.state.buffers.get(id)?.path.clone()?;
                    Some(PersistedLargeFileOverride {
                        path,
                        mode: mode_name_large(mode).into(),
                    })
                })
                .take(STATE_ENTRY_LIMIT)
                .collect(),
            watches: self.debugger_session.persisted_watches(),
            metadata,
        };
        if let Err(error) = self.persistence.save(state, self.persistence_policy()) {
            self.ui.config_diagnostic = Some(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_persistence_events(&mut self) {
        for error in self.persistence.poll_undo_saves() {
            self.show_error(error);
        }
        self.read_persistence_events();
        if !self.persistence.pending() {
            self.tooling.finish_restore();
            self.debugger_session.finish_restore();
        }
        // A project with no saved session file finishes loading without any event.
        if self.session_load_requested && !self.persistence.pending() {
            self.session_load_requested = false;
            self.notify_message(self.message("session_load_none", &[]), false);
        }
    }
}

impl App {
    fn read_persistence_events(&mut self) {
        while let Some(event) = self.persistence.poll() {
            match event {
                Ok(mut state) => {
                    let session = state.session.clone();
                    self.tooling.restore(&state.metadata);
                    if let Err(error) = self
                        .configuration
                        .restore_theme(state.metadata.get("ui.theme.snapshot").map(String::as_str))
                    {
                        self.ui.config_diagnostic = Some(error);
                    }
                    if let Some(session) = &session {
                        self.report_unavailable_persisted_content(session);
                    }
                    self.persistence.restore_history(&mut state);
                    self.debugger_session.restore(
                        state.breakpoints,
                        state.watches,
                        &self.workspace,
                    );
                    self.documents.restore_policy(
                        state
                            .large_file_overrides
                            .into_iter()
                            .map(|entry| (entry.path, entry.mode)),
                    );
                    self.input
                        .restore_history(state.command_history, state.search_history);
                    let requested = std::mem::take(&mut self.session_load_requested);
                    if let Some(session) = session {
                        self.session_restore_notice = requested;
                        self.restore_session(session);
                    } else if requested {
                        self.notify_message(self.message("session_load_none", &[]), false);
                    }
                }
                Err(error) => {
                    self.session_load_requested = false;
                    self.ui.config_diagnostic = Some(error);
                }
            }
        }
        if self.persistence.take_save_request() {
            self.save_persistence();
        }
        if self.persistence.take_saved_notice() {
            self.notify_message(self.message("session_saved", &[]), false);
        }
    }
}

impl App {
    pub(in crate::app) fn report_unavailable_persisted_content(
        &mut self,
        session: &PersistedSession,
    ) {
        // ponytail: production 0.2 has no source-owned restore callback; report
        // unavailable descriptors rather than inventing host-side provider authority.
        let unavailable = session.unavailable_virtual_descriptors();
        if !unavailable.is_empty() {
            self.ui.config_diagnostic = Some(format!(
                "persistent virtual content unavailable under production API 0.2; continuing without: {}",
                unavailable.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }
}

impl App {
    pub(in crate::app) fn restore_session(&mut self, session: PersistedSession) {
        let empty_rules = session.window_rules.clone();
        let paths = match self.persistence.begin_restore(
            session,
            RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            },
            STATE_ENTRY_LIMIT,
        ) {
            Ok(Some(paths)) => paths,
            Ok(None) => {
                self.session_restore_notice = false;
                return;
            }
            Err(error) => {
                self.session_restore_notice = false;
                self.ui.config_diagnostic = Some(error);
                return;
            }
        };
        if paths.is_empty() {
            if let Err(error) = self.apply_persisted_window_rules(&empty_rules) {
                self.ui.config_diagnostic = Some(format!(
                    "persistent window layout rejected transactionally: {error}"
                ));
                self.session_restore_notice = false;
            } else {
                self.notify_session_restored();
            }
            return;
        }
        self.submit_restore_reads();
    }
}

impl App {
    pub(in crate::app) fn submit_restore_reads(&mut self) {
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect::<Vec<_>>();
        while let Some(path) = self.persistence.next_restore_path() {
            match self.documents.check_admission() {
                Ok(()) => {}
                Err(crate::files::VfsError::QueueFull) => return,
                Err(error) => {
                    self.cancel_session_restore();
                    self.show_error(error);
                    return;
                }
            }
            let Some(read) = self.persistence.restore_read(
                &path,
                RestoreContext {
                    editor: &self.state,
                    workspace: &self.workspace,
                    input: &self.input,
                    windows: &self.window_session,
                    configuration: self.configuration.binding_revision(),
                },
            ) else {
                self.cancel_session_restore();
                return;
            };
            match self.documents.read_scoped(path, roots.clone()) {
                Ok(id) => {
                    if let Err(error) = read.register(id) {
                        self.cancel_session_restore();
                        self.show_error(error);
                        return;
                    }
                }
                Err(crate::files::VfsError::QueueFull) => return,
                Err(error) => {
                    drop(read);
                    self.cancel_session_restore();
                    self.show_error(error);
                    return;
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn cancel_session_restore(&mut self) {
        self.session_restore_notice = false;
        for id in self.persistence.cancel_restore() {
            self.documents.cancel_request(id);
        }
    }
}

impl App {
    pub(in crate::app) fn finish_session_restore(&mut self, ready: RestoreReady) {
        let result = ready
            .prepare(RestoreContext {
                editor: &self.state,
                workspace: &self.workspace,
                input: &self.input,
                windows: &self.window_session,
                configuration: self.configuration.binding_revision(),
            })
            .and_then(
                |RestorePrepared {
                     draft,
                     paths,
                     window_rules,
                     content_descriptors,
                     pinned,
                 }| {
                    let terminals = self.terminal_session.descriptions();
                    let descriptions = NativeDescriptions {
                        signature: self.native_surface_signature(self.window_session.editor_ids()),
                        explorer_title: &self.message("explorer_title", &[]),
                        dashboard_title: &self.configuration.dashboard().identity,
                        picker_title: self
                            .ui
                            .picker
                            .as_ref()
                            .map_or("Picker", |picker| &picker.title),
                        terminals: &terminals,
                    };
                    let current = self.state.version();
                    let catalog = self.extension.contributions().version();
                    let transition = self.window_session.prepare_restore_transition(
                        draft,
                        (&current, catalog.clone()),
                        SCRATCH_LABEL,
                        descriptions,
                        &self.extension.window_generations(),
                        RestoreWindowPolicy {
                            rules: &window_rules,
                            descriptors: &content_descriptors,
                        },
                    )?;
                    let state = self
                        .window_session
                        .accept_editor_transition(transition, &current, &catalog)?;
                    Ok((state, paths, pinned))
                },
            );
        match result {
            Ok((state, paths, pinned)) => {
                *self.state = state;
                let retired_confirmation = self.extension.reset_runtime();
                if retired_confirmation
                    && self
                        .input
                        .prompt()
                        .is_some_and(|prompt| prompt.kind == PromptKind::PluginConfirmation)
                {
                    self.close_prompt();
                }
                self.ui.plugin_surfaces.clear();
                self.ui.private_surfaces.clear();
                self.set_dashboard_visibility(false);
                let order = paths
                    .iter()
                    .filter_map(|path| {
                        self.state
                            .buffers
                            .iter()
                            .find(|(_, buffer)| buffer.path.as_ref() == Some(path))
                            .map(|(id, _)| id)
                    })
                    .collect();
                for path in paths {
                    self.record_recent_file(path);
                }
                self.ensure_cursor_visible();
                self.reconcile_editor_windows();
                let pinned = self
                    .state
                    .buffers
                    .iter()
                    .filter(|(_, buffer)| {
                        buffer
                            .path
                            .as_ref()
                            .is_some_and(|path| pinned.contains(path))
                    })
                    .map(|(id, _)| id)
                    .collect();
                self.window_session.restore_pins(pinned);
                self.window_session.restore_content_order(order);
                self.notify_session_restored();
            }
            Err(error) => {
                self.session_restore_notice = false;
                self.show_error(error);
            }
        }
    }
}

impl App {
    fn notify_session_restored(&mut self) {
        if std::mem::take(&mut self.session_restore_notice) {
            self.notify_message(self.message("session_restored", &[]), false);
        }
    }
}

impl App {
    pub(in crate::app) fn record_recent_file(&mut self, path: PathBuf) {
        self.persistence.record_recent(path, RECENT_FILE_LIMIT);
    }
}
