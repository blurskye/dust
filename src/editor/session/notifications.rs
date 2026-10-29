use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::Config,
    telemetry::{CrashSnapshotWriter, LiveCrashSnapshot, LogLevel, LogPolicy, StructuredLogs},
};

/// Owns notification insertion, expiry, history and telemetry writers.
/// The renderer receives a projection, never this owner's mutable state.
pub(super) struct NoticeSession {
    current: Option<(bool, String)>,
    history: VecDeque<(bool, String)>,
    deadline: Option<Instant>,
    logs: StructuredLogs,
    crash_snapshot: CrashSnapshotWriter,
    next_crash_snapshot: Instant,
    /// The last snapshot written, without its timestamp: an unchanged one is not rewritten.
    written_crash_snapshot: Option<LiveCrashSnapshot>,
    terminal_capabilities: String,
}

impl NoticeSession {
    pub(super) fn new(config: &Config, persist: bool, crash_path: PathBuf, now: Instant) -> Self {
        Self {
            current: None,
            history: VecDeque::new(),
            deadline: None,
            logs: make_logs(config, persist),
            crash_snapshot: CrashSnapshotWriter::start(crash_path),
            next_crash_snapshot: now,
            written_crash_snapshot: None,
            terminal_capabilities: "pending".into(),
        }
    }

    pub(super) fn notify(&mut self, message: String, error: bool, now: Instant, timeout: Duration) {
        // Repeated status messages do not perpetually postpone expiry.
        if self
            .current
            .as_ref()
            .is_none_or(|(_, previous)| previous != &message)
        {
            if self
                .history
                .back()
                .is_none_or(|(_, previous)| previous != &message)
            {
                self.history.push_back((error, message.clone()));
                if self.history.len() > 200 {
                    self.history.pop_front();
                }
            }
            self.deadline = (!timeout.is_zero()).then_some(now + timeout);
        }
        self.current = Some((error, message));
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
        self.deadline = None;
    }

    pub(super) fn tick(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(super) fn current(&self) -> Option<(bool, &str)> {
        self.current
            .as_ref()
            .map(|(error, message)| (*error, message.as_str()))
    }

    pub(super) fn history(&self) -> impl DoubleEndedIterator<Item = &(bool, String)> {
        self.history.iter()
    }

    pub(super) fn configure_logs(&mut self, config: &Config, persist: bool) {
        self.logs = make_logs(config, persist);
    }

    pub(super) fn record(
        &mut self,
        level: LogLevel,
        subsystem: &str,
        event: &str,
        fields: BTreeMap<String, String>,
    ) {
        self.logs.record(level, subsystem, event, fields);
    }

    pub(super) fn set_terminal_capabilities(&mut self, capabilities: String) {
        self.terminal_capabilities = capabilities;
    }

    pub(super) fn terminal_capabilities(&self) -> &str {
        &self.terminal_capabilities
    }

    pub(super) fn health_due(&self, now: Instant) -> bool {
        now >= self.next_crash_snapshot
    }

    pub(super) fn submit_health(
        &mut self,
        mut snapshot: LiveCrashSnapshot,
        now: Instant,
        wall_time: SystemTime,
    ) {
        if !self.health_due(now) {
            return;
        }
        self.next_crash_snapshot = now + Duration::from_secs(1);
        if self.written_crash_snapshot.as_ref() == Some(&snapshot) {
            return;
        }
        self.written_crash_snapshot = Some(snapshot.clone());
        snapshot.timestamp_ms = wall_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let _ = self.crash_snapshot.submit(snapshot);
    }
}

fn make_logs(config: &Config, persist: bool) -> StructuredLogs {
    let policy = &config.observability;
    StructuredLogs::new(
        LogPolicy {
            maximum_records: policy.maximum_records,
            maximum_field_bytes: policy.maximum_field_bytes,
            maximum_file_bytes: policy.maximum_file_bytes,
            rotations: policy.rotations,
            sensitive_keys: policy.sensitive_keys.clone(),
        },
        (persist && policy.enabled)
            .then(|| crate::config::user_state_directory().join(&policy.file_name)),
        policy.queue_capacity,
    )
}

#[cfg(test)]
mod tests {
    use super::NoticeSession;
    use std::time::{Duration, Instant};

    #[test]
    fn explicit_insertion_owns_history_and_expiry_without_observing_ui() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(
            &crate::config::load_bundled_config().config,
            false,
            root.path().join("health.json"),
            now,
        );
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
    }

    #[test]
    fn history_retains_only_the_latest_two_hundred_notices() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(
            &crate::config::load_bundled_config().config,
            false,
            root.path().join("health.json"),
            now,
        );
        for index in 0..205 {
            notices.notify(index.to_string(), false, now, Duration::ZERO);
        }
        assert_eq!(notices.history().count(), 200);
        assert_eq!(notices.history().next().unwrap().1, "5");
        assert_eq!(notices.history().next_back().unwrap().1, "204");
    }
}
