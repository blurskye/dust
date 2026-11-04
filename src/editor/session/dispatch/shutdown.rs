use super::{Duration, Instant, InteractiveApplication, InteractiveError, VfsError};

impl InteractiveApplication {
    pub(super) fn finish_shutdown(&mut self) -> Result<(), InteractiveError> {
        // The existing two-second exit budget is shared, not extended. Owner
        // methods admit/drain/signal only; no worker join runs on this thread.
        self.finish_shutdown_before(Instant::now() + Duration::from_secs(2))
    }
    pub(super) fn finish_shutdown_before(
        &mut self,
        deadline: Instant,
    ) -> Result<(), InteractiveError> {
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.reject_workspace_proposal();
        }
        self.cancel_session_restore();
        let discarded = self.extension.close_edits();
        if discarded != 0 {
            self.notices.record(
                crate::telemetry::LogLevel::Info,
                "plugins",
                "discarded-result-obligations-at-shutdown",
                std::collections::BTreeMap::from([("count".into(), discarded.to_string())]),
            );
        }
        for notice in self.workspace_edits.discard_staged_resources("shutdown") {
            self.show_error(notice);
        }
        self.retire_explorer_intents();
        for name in self.tasks.names() {
            if let Ok(owner) = crate::workspace::DiagnosticOwner::new(format!("task:{name}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.language.close_preparation();
        self.debugger_session.begin_shutdown();
        self.tasks.begin_shutdown();
        self.terminal_session.begin_shutdown();
        self.reject_terminal_responses("terminal shutdown requested");
        self.drain_linter_sources();
        self.documents.retire_startup();
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
                self.show_error(error);
            }
            self.drain_job_events(Some(deadline));
            self.process_pty_events();
            self.process_vfs_results();
            self.process_trust_events();
            self.queue_dirty_recovery_before_exit();
            for error in self.language.flush_edit_replies() {
                self.show_error(error);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.documents.close_admission();
        let unwritten = self
            .recovery
            .unwritten(&self.state, &self.configuration.settings().recovery);
        if unwritten != 0 {
            self.show_error(format!("{unwritten} dirty buffer revisions could not be journaled before the shared shutdown deadline"));
        }
        if self.language.preparation_pending() {
            self.show_error("Nix workspace preparation remains at shared shutdown deadline; filesystem outcome unknown");
        }
        let unfinished_reads = self
            .tasks
            .finish_linter_obligations("linter source outcome unknown at shared shutdown deadline");
        if unfinished_reads != 0 {
            self.show_error(format!(
                "{unfinished_reads} linter source reads unfinished at shutdown"
            ));
        }
        self.drain_linter_sources();
        let unfinished_tasks = self.tasks.abandon();
        if unfinished_tasks != 0 {
            self.show_error(format!("{unfinished_tasks} task outcomes are unknown at shutdown; admitted work may still complete"));
        }
        for outcome in self.terminal_session.expire_shutdown() {
            if let super::TerminalUpdate::Unknown(id, message) = outcome {
                self.show_error(format!("terminal #{} outcome is Unknown: {message}", id.0));
            }
        }
        if self.terminal_session.pending() != 0 {
            self.show_error(format!("{} PTY worker outcomes remain at shared shutdown deadline; detached I/O or descendants may remain",self.terminal_session.pending()));
        }
        if self.debugger_session.pending() != 0 {
            self.show_error(format!("{} DAP worker groups/reply receipts remain at shared shutdown deadline; I/O, reply and descendant outcomes unknown",self.debugger_session.pending()));
        }
        let incomplete = self.documents.has_work() || unwritten != 0;
        // Keep admitted resource outcomes owned through the drain; only retire
        // unfinished publication after the existing deadline, never claim rollback.
        self.finish_workspace_resource_obligations("shared shutdown deadline");
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
        while (self.persistence.pending() || !self.documents.worker_finished())
            && Instant::now() < deadline
        {
            self.process_persistence_events();
            std::thread::sleep(Duration::from_millis(2));
        }
        self.process_trust_events();
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
        if !self.finish_agent_history(deadline) {
            return Err(InteractiveError::Vfs(VfsError::Io {
                operation: "finish agent history shutdown",
                path: self.state.workspace_root.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "agent history writes did not finish",
                ),
            }));
        }
        self.persistence.shutdown_result().map_err(|message| {
            InteractiveError::Vfs(VfsError::Io {
                operation: "finish persistence shutdown",
                path: self.state.workspace_root.clone(),
                source: std::io::Error::other(message),
            })
        })
    }
}
