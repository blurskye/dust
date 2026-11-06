use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::telemetry::CrashSnapshotWriter;

use crate::telemetry::LiveCrashSnapshot;

use crate::telemetry::LogLevel;

use crate::telemetry::LogPolicy;

use crate::telemetry::StructuredLogs;

/// Owns notification insertion, expiry, history and telemetry writers.
/// The renderer receives a projection, never this owner's mutable state.
pub(super) struct NoticeSession {
    current: Option<(bool, String)>,
    /// A notice to take down when what it announced is over.
    passing: Option<String>,
    /// The current notice is a warning: something degraded, but the work went on.
    warning: bool,
    history: VecDeque<(bool, String)>,
    deadline: Option<Instant>,
    logs: StructuredLogs,
    crash_snapshot: CrashSnapshotWriter,
    next_crash_snapshot: Instant,
    crash_snapshot_pending: bool,
    /// The last snapshot written, without its timestamp: an unchanged one is not rewritten.
    written_crash_snapshot: Option<LiveCrashSnapshot>,
    terminal_capabilities: String,
}

impl NoticeSession {
    pub(in crate::app) fn new(persist: bool, crash_path: PathBuf, now: Instant) -> Self {
        Self {
            current: None,
            passing: None,
            warning: false,
            history: VecDeque::new(),
            deadline: None,
            logs: make_logs(persist),
            crash_snapshot: CrashSnapshotWriter::start(crash_path),
            next_crash_snapshot: now,
            crash_snapshot_pending: false,
            written_crash_snapshot: None,
            terminal_capabilities: "pending".into(),
        }
    }

    pub(in crate::app) fn notify(&mut self, message: String, error: bool, now: Instant, timeout: Duration) {
        // Repeated status messages do not perpetually postpone expiry.
        if self
            .current
            .as_ref()
            .is_none_or(|(previous_error, previous)| {
                *previous_error != error || previous != &message
            })
        {
            if self
                .history
                .back()
                .is_none_or(|(previous_error, previous)| {
                    *previous_error != error || previous != &message
                })
            {
                self.history.push_back((error, message.clone()));
                if self.history.len() > 200 {
                    self.history.pop_front();
                }
            }
            self.deadline = (!timeout.is_zero()).then_some(now + timeout);
        }
        self.current = Some((error, message));
        self.warning = false;
    }

    pub(super) fn warn(&mut self, message: String, now: Instant, timeout: Duration) {
        self.notify(message, false, now, timeout);
        self.warning = true;
    }

    pub(super) fn is_warning(&self) -> bool {
        self.current.is_some() && self.warning
    }

    pub(in crate::app) fn clear(&mut self) {
        self.current = None;
        self.deadline = None;
    }

    /// Remembers the current notice as one that stops being true, such as "starting".
    pub(in crate::app) fn mark_current_passing(&mut self) {
        self.passing = self.current.as_ref().map(|(_, message)| message.clone());
    }

    /// Takes down the passing notice once it has passed, unless another has replaced it.
    pub(in crate::app) fn clear_passing(&mut self) {
        if self.passing.take().is_some_and(|passing| {
            self.current
                .as_ref()
                .is_some_and(|(_, current)| *current == passing)
        }) {
            self.clear();
        }
    }

    pub(in crate::app) fn tick(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(in crate::app) fn current(&self) -> Option<(bool, &str)> {
        self.current
            .as_ref()
            .map(|(error, message)| (*error, message.as_str()))
    }

    pub(in crate::app) fn history(&self) -> impl DoubleEndedIterator<Item = &(bool, String)> {
        self.history.iter()
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

#[cfg(test)]
mod tests {
    use crate::app::notifications::NoticeSession;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    fn failed_health_snapshot_is_reported_and_unchanged_health_is_retried() {
        use crate::telemetry::LiveCrashSnapshot;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("health.json");
        std::fs::create_dir(&path).unwrap();
        let start = Instant::now();
        let mut notices = NoticeSession::new(false, path.clone(), start);
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

    #[test]
    fn explicit_insertion_owns_history_and_expiry_without_observing_ui() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(false, root.path().join("health.json"), now);
        notices.notify("first".into(), false, now, Duration::from_secs(1));
        notices.notify(
            "first".into(),
            false,
            now + Duration::from_millis(900),
            Duration::from_secs(1),
        );
        assert_eq!(notices.history().count(), 1);
        assert!(!notices.tick(now + Duration::from_millis(999)));
        assert!(notices.tick(now + Duration::from_secs(1)));
        assert_eq!(notices.current(), None);
        assert_eq!(notices.history().count(), 1);
        notices.notify("error".into(), true, now, Duration::ZERO);
        assert!(!notices.tick(now + Duration::from_secs(60)));
        assert_eq!(notices.current(), Some((true, "error")));
        notices.clear();
        assert_eq!(notices.current(), None);
        notices.warn("offline".into(), now, Duration::ZERO);
        assert!(notices.is_warning());
        notices.notify("back".into(), false, now, Duration::ZERO);
        assert!(!notices.is_warning());
    }

    #[test]
    fn changed_severity_records_a_new_notice_and_restarts_expiry() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(false, root.path().join("health.json"), now);
        notices.notify("offline".into(), false, now, Duration::from_secs(1));
        notices.notify(
            "offline".into(),
            true,
            now + Duration::from_millis(900),
            Duration::from_secs(1),
        );
        assert_eq!(notices.current(), Some((true, "offline")));
        assert_eq!(
            notices.history().cloned().collect::<Vec<_>>(),
            vec![(false, "offline".into()), (true, "offline".into()),]
        );
        assert!(!notices.tick(now + Duration::from_secs(1)));
        assert!(notices.tick(now + Duration::from_millis(1900)));
    }

    #[test]
    fn history_retains_only_the_latest_two_hundred_notices() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(false, root.path().join("health.json"), now);
        for index in 0..205 {
            notices.notify(index.to_string(), false, now, Duration::ZERO);
        }
        assert_eq!(notices.history().count(), 200);
        assert_eq!(notices.history().next().unwrap().1, "5");
        assert_eq!(notices.history().next_back().unwrap().1, "204");
    }
}

use crate::app::App;
use crate::app::Path;
use crate::app::Duration;




/// How long a notice stays on screen.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(4);
impl App {

    pub(in crate::app) fn notify_message(&mut self, message: String, error: bool) {
        self.notices
            .notify(message, error, self.clock.monotonic(), NOTICE_TIMEOUT);
        self.project_notice();
    }
}

impl App {

    /// A small yellow notice: something is missing, but what could be shown is shown.
    pub(in crate::app) fn notify_warning(&mut self, message: String) {
        self.notices
            .warn(message, self.clock.monotonic(), NOTICE_TIMEOUT);
        self.project_notice();
    }
}

impl App {

    pub(in crate::app) fn clear_notice(&mut self) {
        self.notices.clear();
        self.project_notice();
    }
}

impl App {

    pub(in crate::app) fn project_notice(&mut self) {
        let current = self.notices.current();
        self.ui.notification = current.map(|(_, message)| message.to_owned());
        self.ui.notification_error = current.is_some_and(|(error, _)| error);
        self.ui.notification_warning = self.notices.is_warning();
    }
}

impl App {


    pub(in crate::app) fn update_notification_timer(&mut self) {
        if self.notices.tick(self.clock.monotonic()) {
            self.project_notice();
        }
    }
}

impl App {

    /// A path as the reader knows it in the project, or whole when it lies outside.
    /// A short name for a notification: outside the workspace a full path would fill the toast.
    pub(in crate::app) fn notice_file_label(&self, path: &std::path::Path) -> String {
        let label = self.workspace_relative_label(path);
        if std::path::Path::new(&label).is_absolute() {
            path.file_name()
                .map_or(label, |name| name.to_string_lossy().into_owned())
        } else {
            label
        }
    }
}

impl App {


    pub(in crate::app) fn show_error(&mut self, error: impl std::fmt::Display) {
        self.notify_message(error.to_string(), true);
    }
}
