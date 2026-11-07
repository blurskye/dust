//! Ordered application shutdown and worker cleanup.

use crate::app::App;
use crate::app::InteractiveError;
use crate::files::VfsError;
use std::time::Duration;
use std::time::Instant;

impl App {
    pub(in crate::app) fn finish_shutdown(&mut self) -> Result<(), InteractiveError> {
        // Workers share a two-second exit budget; none are joined on this thread.
        self.finish_shutdown_before(Instant::now() + Duration::from_secs(2))
    }
    pub(in crate::app) fn finish_shutdown_before(
        &mut self,
        deadline: Instant,
    ) -> Result<(), InteractiveError> {
        let mut diagnostics = Vec::new();
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.cancel_session_restore();
        let discarded = self.extension.close_edits();
        if discarded != 0 {
            self.health.record(
                crate::telemetry::LogLevel::Info,
                "plugins",
                "discarded-pending-results-at-shutdown",
                std::collections::BTreeMap::from([("count".into(), discarded.to_string())]),
            );
        }
        for notice in self.workspace_edits.discard_staged_resources("shutdown") {
            diagnostics.push(notice.to_string());
        }
        self.cancel_explorer_intents();
        for name in self.tasks.names() {
            if let Ok(owner) = crate::workspace::DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.language.close_preparation();
        self.debugger_session.begin_shutdown();
        self.tasks.begin_shutdown();
        self.shutdown_plugin_system();
        self.terminal_session.begin_shutdown();
        self.reject_terminal_responses("terminal shutdown requested");
        self.drain_linter_sources();
        self.documents.cancel_startup();
        self.queue_dirty_recovery_before_exit();
        while (self
            .recovery
            .unwritten(&self.state, &self.configuration.settings().recovery)
            != 0
            || self.documents.has_work()
            || self.language.preparation_pending()
            || self.language.pending_replies()
            || self.tasks.running_count() != 0
            || self.terminal_session.pending() != 0
            || self.debugger_session.pending() != 0
            || self.workspace.persist_pending())
            && Instant::now() < deadline
        {
            for error in self.debugger_session.reply_failures() {
                diagnostics.push(error.to_string());
            }
            self.drain_job_events(Some(deadline));
            self.process_pty_events();
            self.process_vfs_results();
            self.process_trust_events();
            self.queue_dirty_recovery_before_exit();
            for error in self.language.flush_edit_replies() {
                diagnostics.push(error.to_string());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.documents.stop_accepting();
        let unwritten = self
            .recovery
            .unwritten(&self.state, &self.configuration.settings().recovery);
        if unwritten != 0 {
            diagnostics.push(format!("{unwritten} dirty buffer revisions could not be journaled before the shared shutdown deadline"));
            let oversized = self
                .state
                .buffers
                .values()
                .filter(|buffer| {
                    buffer.dirty
                        && !crate::files::recovery_size_allowed(buffer.text.len_bytes() as u64)
                })
                .count();
            if oversized != 0 {
                diagnostics.push(format!("{oversized} dirty buffers exceed the recovery journal size limit; their unsaved contents cannot be recovered"));
            }
        }
        if self.language.preparation_pending() {
            diagnostics.push("Nix workspace preparation remains at shared shutdown deadline; filesystem outcome unknown".into());
        }
        let unfinished_reads = self
            .tasks
            .finish_linter_reads("linter source outcome unknown at shared shutdown deadline");
        if unfinished_reads != 0 {
            diagnostics.push(format!(
                "{unfinished_reads} linter source reads unfinished at shutdown"
            ));
        }
        self.drain_linter_sources();
        let unfinished_tasks = self.tasks.abandon();
        if unfinished_tasks != 0 {
            diagnostics.push(format!("{unfinished_tasks} task outcomes are unknown at shutdown; accepted work may still complete"));
        }
        for outcome in self.terminal_session.expire_shutdown() {
            if let crate::app::terminal::TerminalUpdate::Unknown(id, message) = outcome {
                diagnostics.push(format!("terminal #{} outcome is Unknown: {message}", id.0));
            }
        }
        if self.terminal_session.pending() != 0 {
            diagnostics.push(format!("{} PTY worker outcomes remain at shared shutdown deadline; detached I/O or descendants may remain",self.terminal_session.pending()));
        }
        if self.debugger_session.pending() != 0 {
            diagnostics.push(format!("{} DAP worker groups/reply deliveries remain at shared shutdown deadline; I/O, reply and descendant outcomes unknown",self.debugger_session.pending()));
        }
        let incomplete = self.documents.has_work() || unwritten != 0;
        // Keep tracking resource results until the deadline. Dropping unfinished
        // results does not roll back filesystem operations.
        self.finish_workspace_resources("shared shutdown deadline");
        self.documents.stop_worker();
        self.persistence
            .begin_shutdown(self.configuration.settings().persistence.enabled);
        // Drain an earlier load/save without applying a late restore at exit.
        while self.persistence.pending() && Instant::now() < deadline {
            self.process_persistence_events();
            std::thread::sleep(Duration::from_millis(2));
        }
        if !incomplete && Instant::now() < deadline {
            self.save_persistence();
        }
        while (self.persistence.pending()
            || self.persistence.undo_pending()
            || !self.documents.worker_finished())
            && Instant::now() < deadline
        {
            self.process_persistence_events();
            std::thread::sleep(Duration::from_millis(2));
        }
        for error in self.persistence.poll_undo_saves() {
            self.show_error(error);
        }
        self.process_trust_events();
        let result = (|| {
            self.workspace.trust_shutdown_result().map_err(|message| {
                InteractiveError::Vfs(VfsError::Io {
                    operation: "finish workspace trust shutdown",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::other(message),
                })
            })?;
            if self.language.pending_replies() {
                return Err(InteractiveError::Vfs(VfsError::Io {
                    operation: "finish requesting-server edit responses",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "requesting-server responses were not acknowledged before the shared exit deadline",
                    ),
                }));
            }
            if incomplete || !self.documents.worker_finished() {
                return Err(InteractiveError::Vfs(VfsError::Io {
                    operation: "finish document shutdown",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "filesystem work did not quiesce; an already-running operation may still complete",
                    ),
                }));
            }
            self.finish_agent_history(deadline).map_err(|message| {
                InteractiveError::Vfs(VfsError::Io {
                    operation: "finish agent history shutdown",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::other(message),
                })
            })?;
            self.persistence.undo_shutdown_result().map_err(|message| {
                InteractiveError::Vfs(VfsError::Io {
                    operation: "finish undo history shutdown",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::other(message),
                })
            })?;
            self.persistence.shutdown_result().map_err(|message| {
                InteractiveError::Vfs(VfsError::Io {
                    operation: "finish persistence shutdown",
                    path: self.state.workspace_root.clone(),
                    source: std::io::Error::other(message),
                })
            })
        })();
        if diagnostics.is_empty() {
            return result;
        }
        for diagnostic in &diagnostics {
            self.show_error(diagnostic);
        }
        if let Err(error) = result {
            diagnostics.push(error.to_string());
        }
        Err(InteractiveError::Vfs(VfsError::Io {
            operation: "finish shutdown",
            path: self.state.workspace_root.clone(),
            source: std::io::Error::other(diagnostics.join("; ")),
        }))
    }
}

use crate::app::commands::AppEffect;

impl App {
    pub(in crate::app) fn process_shutdown_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::Quit { force } => {
                if !force && self.state.buffers.iter().any(|(_, buffer)| buffer.dirty) {
                    self.show_error(self.message("unsaved_quit", &[]));
                } else {
                    if force {
                        self.queue_dirty_recovery_before_exit();
                    }
                    self.should_quit = true;
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
