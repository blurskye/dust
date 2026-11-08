//! Persistence I/O, history, session capture and pending restores.
//! Dispatch composes other owners' snapshots and consumes checked editor/window plans.
use crate::persistence::PersistedState;
use crate::persistence::PersistenceError;
use crate::persistence::PersistenceEvent;
use crate::persistence::PersistenceId;
use crate::persistence::PersistencePolicy;
use crate::persistence::PersistenceWorker;
use std::path::PathBuf;

pub(in crate::app) const RECENT_FILE_LIMIT: usize = 256;
pub(in crate::app) const MARK_LIMIT: usize = 256;
pub(in crate::app) const JUMP_LIMIT: usize = 1024;
pub(in crate::app) const CHANGE_LIMIT: usize = 1024;
pub(in crate::app) const STATE_ENTRY_LIMIT: usize = 10_000;
pub(in crate::app) const STATE_MAXIMUM_BYTES: usize = 4 * 1024 * 1024;
pub(in crate::app) const STATE_FILE: &str = "session.json";
pub(in crate::app) const PERSISTENCE_QUEUE_CAPACITY: usize = 4;

mod session;
pub(in crate::app) use session::RestoreContext;
pub(in crate::app) use session::RestorePrepared;
pub(in crate::app) use session::RestoreReady;
pub(in crate::app) use session::RestoreUpdate;
#[cfg(test)]
pub(in crate::app) use session::capture_persisted_layout;

pub(in crate::app) struct Persistence {
    load_source: Option<session::RestoreSource>,
    restore: Option<session::PendingRestore>,
    recent_files: Vec<PathBuf>,
    marks: Vec<crate::persistence::PersistedMark>,
    mark_sources: std::collections::HashMap<
        String,
        (
            crate::editor::BufferId,
            crate::editor::text::TextVersion,
            crate::editor::text::BufferRevision,
        ),
    >,
    jumps: Vec<crate::persistence::PersistedLocation>,
    /// Where `ctrl-o`/`ctrl-i` stand in `jumps`; `None` is past the newest entry.
    jump_index: Option<usize>,
    changes: Vec<crate::persistence::PersistedLocation>,
    worker: PersistenceWorker,
    path: PathBuf,
    pending: Option<(PersistenceId, bool)>,
    save_requested: bool,
    save_notice: bool,
    saved_notice: bool,
    failure: Option<String>,
    undo_saves: Vec<(PathBuf, std::sync::mpsc::Receiver<Result<(), String>>)>,
    undo_failures: std::collections::BTreeMap<PathBuf, String>,
    closing: bool,
    save_expected: bool,
    saved_on_exit: bool,
    exit_request: Option<PersistenceId>,
    navigation_reads:
        std::collections::HashMap<crate::files::RequestId, crate::persistence::PersistedLocation>,
}
impl Persistence {
    pub(in crate::app) fn undo_directory(&self) -> PathBuf {
        self.path.parent().expect("state directory").join("undo")
    }
    pub(in crate::app) fn new(worker: PersistenceWorker, path: PathBuf) -> Self {
        Self {
            load_source: None,
            restore: None,
            recent_files: Vec::new(),
            marks: Vec::new(),
            mark_sources: Default::default(),
            jumps: Vec::new(),
            jump_index: None,
            changes: Vec::new(),
            worker,
            path,
            pending: None,
            save_requested: false,
            save_notice: false,
            saved_notice: false,
            failure: None,
            undo_saves: Vec::new(),
            undo_failures: Default::default(),
            closing: false,
            save_expected: false,
            saved_on_exit: false,
            exit_request: None,
            navigation_reads: Default::default(),
        }
    }
    pub(in crate::app) fn track_undo_save(
        &mut self,
        path: PathBuf,
        reply: std::sync::mpsc::Receiver<Result<(), String>>,
    ) {
        self.undo_saves.push((path, reply));
    }
    pub(in crate::app) fn undo_pending(&self) -> bool {
        !self.undo_saves.is_empty()
    }
    pub(in crate::app) fn poll_undo_saves(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        self.undo_saves.retain(|(path, reply)| {
            let error = match reply.try_recv() {
                Ok(Ok(())) => {
                    self.undo_failures.remove(path);
                    return false;
                }
                Ok(Err(error)) => error,
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    "undo history worker stopped".into()
                }
            };
            let message = format!("could not save undo history: {error}");
            self.undo_failures.insert(path.clone(), message.clone());
            errors.push(message);
            false
        });
        errors
    }
    pub(in crate::app) fn undo_shutdown_result(&self) -> Result<(), String> {
        if self.undo_pending() {
            return Err("undo history writes did not finish before the exit deadline".into());
        }
        self.undo_failures
            .values()
            .next()
            .cloned()
            .map_or(Ok(()), Err)
    }
    pub(in crate::app) fn register_navigation_read(
        &mut self,
        id: crate::files::RequestId,
        location: crate::persistence::PersistedLocation,
    ) {
        self.navigation_reads.insert(id, location);
    }
    pub(in crate::app) fn take_navigation_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<crate::persistence::PersistedLocation> {
        self.navigation_reads.remove(&id)
    }
    pub(in crate::app) fn forget_document_request(&mut self, id: crate::files::RequestId) {
        self.navigation_reads.remove(&id);
    }
    pub(in crate::app) fn request_save_notice(&mut self) {
        self.save_notice = true;
    }
    pub(in crate::app) fn take_saved_notice(&mut self) -> bool {
        std::mem::take(&mut self.saved_notice)
    }
    pub(in crate::app) fn request_save(&mut self) {
        if !self.closing {
            self.save_requested = true;
        }
    }
    pub(in crate::app) fn take_save_request(&mut self) -> bool {
        !self.pending() && !self.closing && std::mem::take(&mut self.save_requested)
    }
    pub(in crate::app) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::app) fn load(&mut self, policy: PersistencePolicy) -> Result<(), String> {
        if self.pending() || self.closing {
            return Err("persistence is busy or closing".into());
        }
        let id = self
            .worker
            .read(self.path.clone(), policy)
            .map_err(|e| self.failed(e))?;
        self.pending = Some((id, false));
        self.failure = None;
        Ok(())
    }
    pub(in crate::app) fn save(
        &mut self,
        state: PersistedState,
        policy: PersistencePolicy,
    ) -> Result<(), String> {
        if self.pending() {
            return Err("persistence is already pending".into());
        }
        self.save_requested = false;
        let id = self
            .worker
            .write(self.path.clone(), state, policy)
            .map_err(|e| self.failed(e))?;
        self.pending = Some((id, true));
        if self.closing {
            self.exit_request = Some(id);
            self.saved_on_exit = false;
        }
        self.failure = None;
        Ok(())
    }
    pub(in crate::app) fn failed(&mut self, error: impl std::fmt::Display) -> String {
        let message = error.to_string().chars().take(1024).collect::<String>();
        self.failure = Some(message.clone());
        message
    }
    pub(in crate::app) fn poll(&mut self) -> Option<Result<Box<PersistedState>, String>> {
        loop {
            let event = match self.worker.try_event() {
                Ok(Some(event)) => event,
                Ok(None) => return None,
                Err(error) => {
                    self.pending.take()?;
                    return Some(Err(self.failed(error)));
                }
            };
            let request_id = match &event {
                PersistenceEvent::Loaded { request_id, .. }
                | PersistenceEvent::Saved { request_id }
                | PersistenceEvent::Failed { request_id, .. } => *request_id,
            };
            let Some((expected, saving)) = self.pending else {
                continue;
            };
            if expected != request_id {
                continue;
            }
            self.pending = None;
            match event {
                PersistenceEvent::Loaded { state, .. } if !saving && !self.closing => {
                    return Some(Ok(state));
                }
                PersistenceEvent::Loaded { .. } if !saving => {}
                PersistenceEvent::Saved { .. } if saving => {
                    self.saved_on_exit = self.exit_request == Some(request_id);
                    if !self.save_requested && std::mem::take(&mut self.save_notice) {
                        self.saved_notice = true;
                    }
                }
                PersistenceEvent::Failed {
                    error: PersistenceError::Io(error),
                    ..
                } if !saving && error.kind() == std::io::ErrorKind::NotFound => {}
                PersistenceEvent::Failed { error, .. } => return Some(Err(self.failed(error))),
                _ => return Some(Err(self.failed("unexpected persistence terminal result"))),
            }
        }
    }
    pub(in crate::app) fn begin_shutdown(&mut self, save_expected: bool) {
        self.closing = true;
        self.save_expected = save_expected;
        self.saved_on_exit = false;
        self.exit_request = None;
    }
    pub(in crate::app) fn shutdown_result(&self) -> Result<(), String> {
        if self.pending() {
            return Err("persistence did not finish before the exit deadline; an accepted write may still complete".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.save_expected && !self.saved_on_exit {
            return Err(
                "exit snapshot was not saved; previous persistent state was not replaced".into(),
            );
        }
        Ok(())
    }
}

pub(in crate::app) mod restore;

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::documents::mode_name_large;
use crate::app::windows::NativeDescriptions;
use crate::app::windows::RestoreWindowPolicy;
use crate::app::windows::WindowCandidate;
use crate::persistence::PersistedLargeFileOverride;
use crate::persistence::PersistedSession;
use crate::persistence::PersistedWindowRule;
use crate::persistence::STATE_VERSION;
use crate::ui::PromptKind;
use crate::ui::render::SCRATCH_LABEL;

impl App {
    fn persistence_policy(&self) -> PersistencePolicy {
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
                signature: self.native_surface_signature(),
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
                signature: self.native_surface_signature(),
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
        self.window_session.accept_current(
            candidate,
            &self.state.window_version(),
            &mut self.state,
        )?;
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
                "persistent virtual content unavailable under production API 0.4; continuing without: {}",
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
            match self.documents.check_capacity() {
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
                        signature: self.native_surface_signature(),
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
                        &self.extension.window_loads(),
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
                let cancelled_confirmation = self.extension.reset_runtime();
                if cancelled_confirmation
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

#[cfg(test)]
pub(in crate::app) mod tests;
