//! Health diagnostics, logging and crash snapshots.

use crate::app::App;
use crate::plugins::PluginStatus;
use crate::telemetry::LiveCrashSnapshot;

impl App {
    pub(in crate::app) fn update_live_crash_snapshot(&mut self) {
        let now = self.clock.monotonic();
        if !self.health.health_due(now) {
            return;
        }
        self.health.submit_health(
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

use crate::app::commands::AppEffect;

use crate::app::commands::register;
use crate::editor::ArgumentSchema;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "health.open",
        "Show health",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::ShowHealth,
    )?;
    register(
        registry,
        "crash.inspect",
        "Inspect previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashInspect,
    )?;
    register(
        registry,
        "crash.clear",
        "Clear previous crash",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::CrashClear,
    )?;
    register(
        registry,
        "health.virtual.open",
        "Open read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualOpen,
    )?;
    register(
        registry,
        "health.virtual.refresh",
        "Refresh read-only health document",
        ContextRequirement::Application,
        ArgumentSchema::default(),
        |_| AppEffect::HealthVirtualRefresh,
    )?;
    Ok(())
}

use crate::telemetry::CrashSnapshotWriter;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::telemetry::LogLevel;
use crate::telemetry::LogPolicy;
use crate::telemetry::StructuredLogs;

pub(in crate::app) struct Health {
    logs: StructuredLogs,
    crash_snapshot: CrashSnapshotWriter,
    next_crash_snapshot: Instant,
    crash_snapshot_pending: bool,
    /// The last snapshot written, without its timestamp: an unchanged one is not rewritten.
    written_crash_snapshot: Option<LiveCrashSnapshot>,
    terminal_capabilities: String,
}
impl Health {
    pub(in crate::app) fn new(persist: bool, crash_path: PathBuf, now: Instant) -> Self {
        Self {
            logs: make_logs(persist),
            crash_snapshot: CrashSnapshotWriter::start(crash_path),
            next_crash_snapshot: now,
            crash_snapshot_pending: false,
            written_crash_snapshot: None,
            terminal_capabilities: "pending".into(),
        }
    }

    pub(in crate::app) fn record(
        &mut self,
        level: LogLevel,
        subsystem: &str,
        event: &str,
        fields: BTreeMap<String, String>,
    ) {
        self.logs.record(level, subsystem, event, fields);
    }

    pub(in crate::app) fn set_terminal_capabilities(&mut self, capabilities: String) {
        self.terminal_capabilities = capabilities;
    }

    pub(in crate::app) fn terminal_capabilities(&self) -> &str {
        &self.terminal_capabilities
    }

    pub(in crate::app) fn health_due(&self, now: Instant) -> bool {
        now >= self.next_crash_snapshot
    }

    pub(in crate::app) fn submit_health(
        &mut self,
        mut snapshot: LiveCrashSnapshot,
        now: Instant,
        wall_time: SystemTime,
    ) {
        if !self.health_due(now) {
            return;
        }
        self.next_crash_snapshot = now + Duration::from_secs(1);
        if let Some(result) = self.crash_snapshot.try_result() {
            self.crash_snapshot_pending = false;
            match result {
                Ok(mut written) => {
                    written.timestamp_ms = 0;
                    self.written_crash_snapshot = Some(written);
                }
                Err(error) => self.record(
                    LogLevel::Error,
                    "telemetry",
                    "crash_snapshot_failed",
                    BTreeMap::from([("error".into(), error)]),
                ),
            }
        }
        if self.crash_snapshot_pending || self.written_crash_snapshot.as_ref() == Some(&snapshot) {
            return;
        }
        snapshot.timestamp_ms = wall_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        match self.crash_snapshot.submit(snapshot) {
            Ok(()) => self.crash_snapshot_pending = true,
            Err(error) => self.record(
                LogLevel::Error,
                "telemetry",
                "crash_snapshot_failed",
                BTreeMap::from([("error".into(), error)]),
            ),
        }
    }
}
/// Structured logs kept in memory and, for a persistent session, in the state directory.
fn make_logs(persist: bool) -> StructuredLogs {
    StructuredLogs::new(
        LogPolicy {
            maximum_records: 2000,
            maximum_field_bytes: 4096,
            maximum_file_bytes: 8 * 1024 * 1024,
            rotations: 3,
            sensitive_keys: [
                "token",
                "password",
                "secret",
                "authorization",
                "source",
                "text",
            ]
            .map(String::from)
            .to_vec(),
        },
        persist.then(|| crate::config::user_state_directory().join("dust.jsonl")),
        256,
    )
}

use crate::app::plugins::VirtualContext;
use crate::ui::render::SCRATCH_LABEL;

impl App {
    pub(in crate::app) fn process_health_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::CrashInspect => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                self.notify_message(
                    self.message("crash_location", &[("path", path.display().to_string())]),
                    path.is_file(),
                );
            }
            AppEffect::CrashClear => {
                let path = crate::config::user_state_directory().join("last-crash.json");
                if let Err(error) = self
                    .recovery
                    .clear_crash(path, self.documents.recovery_io())
                {
                    self.show_error(error);
                }
            }
            AppEffect::ShowHealth => {
                self.notify_message(
                    self.message(
                        "healthy",
                        &[
                            ("buffers", self.state.buffers.len().to_string()),
                            ("workspace", self.state.workspace_root.display().to_string()),
                            ("terminal", self.health.terminal_capabilities().to_owned()),
                            ("jobs", self.tasks.running_count().to_string()),
                            ("ptys", self.terminal_session.running_count().to_string()),
                            ("preflights", self.pending_preflight_count().to_string()),
                            ("tasks", self.tasks.status_count().to_string()),
                            ("plugins", self.extension.statuses().len().to_string()),
                            (
                                "plugin_failures",
                                self.extension
                                    .statuses()
                                    .values()
                                    .filter(|status| {
                                        matches!(status, crate::plugins::PluginStatus::Failed(_))
                                    })
                                    .count()
                                    .to_string(),
                            ),
                            ("packages", self.extension.pending_packages().to_string()),
                            ("lsp", self.language.sessions().len().to_string()),
                            ("persistence", self.persistence.pending().to_string()),
                            ("safe", self.workspace.safe_mode().to_string()),
                            ("dap", self.debugger_session.is_running().to_string()),
                        ],
                    ),
                    false,
                );
            }
            kind @ (AppEffect::HealthVirtualOpen | AppEffect::HealthVirtualRefresh) => {
                let result = self
                    .extension
                    .prepare_health(
                        matches!(kind, AppEffect::HealthVirtualRefresh),
                        self.commands.specs().count(),
                        VirtualContext {
                            editor: &self.state,
                            windows: &self.window_session,
                            scratch: SCRATCH_LABEL,
                        },
                    )
                    .and_then(|staged| {
                        Self::accept_virtual_publication(&mut self.window_session, staged)
                    });
                match result {
                    Ok(state) => {
                        *self.state = state;
                        self.set_dashboard_visibility(false);
                        self.reconcile_editor_windows();
                    }
                    Err(error) => self.show_error(error),
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Health;
    use std::time::{Duration, Instant};
    #[test]
    fn failed_health_snapshot_is_reported_and_unchanged_health_is_retried() {
        use crate::telemetry::LiveCrashSnapshot;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("health.json");
        std::fs::create_dir(&path).unwrap();
        let start = Instant::now();
        let mut notices = Health::new(false, path.clone(), start);
        let snapshot = LiveCrashSnapshot {
            timestamp_ms: 0,
            roots: 1,
            buffers: 2,
            dirty_buffers: 1,
            jobs: 0,
            ptys: 0,
            lsp_sessions: 0,
            dap_running: false,
            pending_effects: 0,
            plugins_failed: 0,
            safe_mode: false,
        };
        let mut tick = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            notices.submit_health(
                snapshot.clone(),
                start + Duration::from_secs(tick),
                std::time::UNIX_EPOCH,
            );
            tick += 1;
            if notices
                .logs
                .snapshot()
                .iter()
                .any(|record| record.event == "crash_snapshot_failed")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "snapshot failure was not reported"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(notices.written_crash_snapshot.is_none());
        std::fs::remove_dir(&path).unwrap();
        loop {
            notices.submit_health(
                snapshot.clone(),
                start + Duration::from_secs(tick),
                std::time::UNIX_EPOCH,
            );
            tick += 1;
            if notices.written_crash_snapshot.as_ref() == Some(&snapshot) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "unchanged health was not retried"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["buffers"], 2);
        assert_eq!(written["dirty_buffers"], 1);
    }
}
