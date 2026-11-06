use crate::app::App;
use crate::app::LiveCrashSnapshot;
use crate::app::PluginStatus;



impl App {

    pub(in crate::app) fn update_live_crash_snapshot(&mut self) {
        let now = self.clock.monotonic();
        if !self.notices.health_due(now) {
            return;
        }
        self.notices.submit_health(
            LiveCrashSnapshot {
                timestamp_ms: 0,
                roots: self.workspace.roots().len(),
                buffers: self.state.buffers.len(),
                dirty_buffers: self
                    .state
                    .buffers
                    .values()
                    .filter(|buffer| buffer.dirty)
                    .count(),
                jobs: self.tasks.running_count(),
                ptys: self.terminal_session.len(),
                lsp_sessions: self.language.sessions().len(),
                dap_running: self.debugger_session.is_running(),
                pending_effects: self
                    .documents
                    .pending_count()
                    .saturating_add(self.pending_preflight_count())
                    .saturating_add(self.tasks.linter_count()),
                plugins_failed: self
                    .extension
                    .statuses()
                    .values()
                    .filter(|status| matches!(status, PluginStatus::Failed(_)))
                    .count(),
                safe_mode: self.workspace.safe_mode(),
            },
            now,
            self.clock.system_time(),
        );
    }
}
