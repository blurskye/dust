use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::telemetry::{
    CrashSnapshotWriter, LiveCrashSnapshot, LogLevel, LogPolicy, StructuredLogs,
};

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
    /// The last snapshot written, without its timestamp: an unchanged one is not rewritten.
    written_crash_snapshot: Option<LiveCrashSnapshot>,
    terminal_capabilities: String,
}

impl NoticeSession {
    pub(super) fn new(persist: bool, crash_path: PathBuf, now: Instant) -> Self {
        Self {
            current: None,
            passing: None,
            warning: false,
            history: VecDeque::new(),
            deadline: None,
            logs: make_logs(persist),
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
        self.warning = false;
    }

    pub(super) fn warn(&mut self, message: String, now: Instant, timeout: Duration) {
        self.notify(message, false, now, timeout);
        self.warning = true;
    }

    pub(super) fn is_warning(&self) -> bool {
        self.current.is_some() && self.warning
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
        self.deadline = None;
    }

    /// Remembers the current notice as one that stops being true, such as "starting".
    pub(super) fn mark_current_passing(&mut self) {
        self.passing = self.current.as_ref().map(|(_, message)| message.clone());
    }

    /// Takes down the passing notice once it has passed, unless another has replaced it.
    pub(super) fn clear_passing(&mut self) {
        if self.passing.take().is_some_and(|passing| {
            self.current.as_ref().is_some_and(|(_, current)| *current == passing)
        }) {
            self.clear();
        }
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

/// Structured logs kept in memory and, for a persistent session, in the state directory.
fn make_logs(persist: bool) -> StructuredLogs {
    StructuredLogs::new(
        LogPolicy {
            maximum_records: 2000,
            maximum_field_bytes: 4096,
            maximum_file_bytes: 8 * 1024 * 1024,
            rotations: 3,
            sensitive_keys: ["token", "password", "secret", "authorization", "source", "text"]
                .map(String::from)
                .to_vec(),
        },
        persist.then(|| crate::config::user_state_directory().join("dust.jsonl")),
        256,
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
        notices.warn("offline".into(), now, Duration::ZERO);
        assert!(notices.is_warning());
        notices.notify("back".into(), false, now, Duration::ZERO);
        assert!(!notices.is_warning());
    }

    #[test]
    fn history_retains_only_the_latest_two_hundred_notices() {
        let root = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut notices = NoticeSession::new(
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
