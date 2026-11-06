//! Logging, traces and crash records.

#![forbid(unsafe_code)]
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::time::UNIX_EPOCH;
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LogRecord {
    pub timestamp_ms: u64,
    pub level: LogLevel,
    pub subsystem: String,
    pub event: String,
    pub fields: BTreeMap<String, String>,
}
#[derive(Clone, Debug)]
pub struct LogPolicy {
    pub maximum_records: usize,
    pub maximum_field_bytes: usize,
    pub maximum_file_bytes: u64,
    pub rotations: usize,
    pub sensitive_keys: Vec<String>,
}
pub struct StructuredLogs {
    records: VecDeque<LogRecord>,
    policy: LogPolicy,
    sender: Option<SyncSender<LogRecord>>,
}
impl StructuredLogs {
    pub fn new(policy: LogPolicy, path: Option<PathBuf>, queue: usize) -> Self {
        let sender = path.map(|path| {
            let (tx, rx) = mpsc::sync_channel::<LogRecord>(queue.clamp(1, 4096));
            let worker_policy = policy.clone();
            thread::Builder::new()
                .name("dust-logs".into())
                .spawn(move || {
                    while let Ok(record) = rx.recv() {
                        let _ = write_record(&path, &record, &worker_policy);
                    }
                })
                .expect("log worker");
            tx
        });
        Self {
            records: VecDeque::new(),
            policy,
            sender,
        }
    }
    pub fn record(
        &mut self,
        level: LogLevel,
        subsystem: &str,
        event: &str,
        mut fields: BTreeMap<String, String>,
    ) {
        for (key, value) in &mut fields {
            if self
                .policy
                .sensitive_keys
                .iter()
                .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
            {
                *value = "[redacted]".into()
            } else {
                value.retain(|character| !character.is_control());
                let mut end = value.len().min(self.policy.maximum_field_bytes);
                while !value.is_char_boundary(end) {
                    end -= 1;
                }
                value.truncate(end);
            }
        }
        let record = LogRecord {
            timestamp_ms: crate::clock::system_time()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            level,
            subsystem: subsystem.chars().take(128).collect(),
            event: event.chars().take(256).collect(),
            fields,
        };
        self.records.push_back(record.clone());
        while self.records.len() > self.policy.maximum_records {
            self.records.pop_front();
        }
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(record);
        }
    }
    pub fn snapshot(&self) -> Vec<LogRecord> {
        self.records.iter().cloned().collect()
    }
}
fn write_record(path: &PathBuf, record: &LogRecord, policy: &LogPolicy) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _lock = crate::files::lock_file(&path.with_extension("jsonl.lock"))?;
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() >= policy.maximum_file_bytes) {
        for index in (1..policy.rotations).rev() {
            let from = path.with_extension(format!("jsonl.{index}"));
            let to = path.with_extension(format!("jsonl.{}", index + 1));
            if let Err(error) = fs::rename(from, to)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error);
            }
        }
        fs::rename(path, path.with_extension("jsonl.1"))?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    file.write_all(&bytes)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LiveCrashSnapshot {
    pub timestamp_ms: u64,
    pub roots: usize,
    pub buffers: usize,
    pub dirty_buffers: usize,
    pub jobs: usize,
    pub ptys: usize,
    pub lsp_sessions: usize,
    pub dap_running: bool,
    pub pending_effects: usize,
    pub plugins_failed: usize,
    pub safe_mode: bool,
}
pub struct CrashSnapshotWriter {
    sender: SyncSender<LiveCrashSnapshot>,
    results: Receiver<Result<LiveCrashSnapshot, String>>,
}
impl CrashSnapshotWriter {
    pub fn start(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (results_sender, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("dust-crash-snapshot".into())
            .spawn(move || {
                while let Ok(snapshot) = receiver.recv() {
                    let result = write_live_snapshot(&path, &snapshot)
                        .map(|()| snapshot)
                        .map_err(|error| format!("could not write crash snapshot: {error}"));
                    if results_sender.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("crash snapshot worker");
        Self { sender, results }
    }
    pub fn try_result(&self) -> Option<Result<LiveCrashSnapshot, String>> {
        self.results.try_recv().ok()
    }
    pub fn submit(&self, snapshot: LiveCrashSnapshot) -> Result<(), String> {
        self.sender.try_send(snapshot).map_err(|error| match error {
            TrySendError::Full(_) => "crash snapshot queue full".into(),
            TrySendError::Disconnected(_) => "crash snapshot worker disconnected".into(),
        })
    }
}
fn write_live_snapshot(path: &PathBuf, snapshot: &LiveCrashSnapshot) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.file_type().is_file() || metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "crash snapshot target must be a regular file",
        ));
    }
    let bytes = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    if bytes.len() > 64 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "crash snapshot size limit",
        ));
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HealthSnapshot {
    pub vfs_pending: usize,
    pub jobs_running: usize,
    pub lsp_sessions: usize,
    pub dap_running: bool,
    pub plugins_failed: usize,
    pub config_degraded: bool,
    pub safe_mode: bool,
    pub recovery_pending: usize,
    pub notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use crate::telemetry::*;

    #[test]
    fn log_fields_are_limited_by_utf8_bytes() {
        let mut logs = StructuredLogs::new(
            LogPolicy {
                maximum_records: 1,
                maximum_field_bytes: 5,
                maximum_file_bytes: 1024,
                rotations: 1,
                sensitive_keys: vec!["secret".into()],
            },
            None,
            1,
        );
        logs.record(
            LogLevel::Info,
            "test",
            "bytes",
            BTreeMap::from([
                ("emoji".into(), "\n😀😀".into()),
                ("mixed".into(), "a😀b".into()),
                ("secret".into(), "password".into()),
            ]),
        );
        let fields = &logs.snapshot()[0].fields;
        assert_eq!(fields["emoji"], "😀");
        assert_eq!(fields["mixed"], "a😀");
        assert_eq!(fields["secret"], "[redacted]");
    }

    #[test]
    fn failed_log_rotation_does_not_append() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dust.jsonl");
        let record = LogRecord {
            timestamp_ms: 0,
            level: LogLevel::Info,
            subsystem: "test".into(),
            event: "rotation".into(),
            fields: BTreeMap::new(),
        };
        let policy = LogPolicy {
            maximum_records: 1,
            maximum_field_bytes: 1,
            maximum_file_bytes: 1,
            rotations: 1,
            sensitive_keys: Vec::new(),
        };
        fs::write(&path, "full").unwrap();
        fs::create_dir(path.with_extension("jsonl.1")).unwrap();
        assert!(write_record(&path, &record, &policy).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"full");
        fs::remove_dir(path.with_extension("jsonl.1")).unwrap();
        write_record(&path, &record, &policy).unwrap();
        assert_eq!(fs::read(path.with_extension("jsonl.1")).unwrap(), b"full");
    }

    #[test]
    fn concurrent_log_writers_keep_records_and_rotation_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dust.jsonl");
        let policy = LogPolicy {
            maximum_records: 1000,
            maximum_field_bytes: 8192,
            maximum_file_bytes: 64 * 1024,
            rotations: 32,
            sensitive_keys: Vec::new(),
        };
        thread::scope(|scope| {
            for writer in 0..4 {
                let path = &path;
                let policy = &policy;
                scope.spawn(move || {
                    for index in 0..50 {
                        write_record(
                            path,
                            &LogRecord {
                                timestamp_ms: 0,
                                level: LogLevel::Info,
                                subsystem: writer.to_string(),
                                event: index.to_string(),
                                fields: BTreeMap::from([("text".into(), "x".repeat(4096))]),
                            },
                            policy,
                        )
                        .unwrap();
                    }
                });
            }
        });
        let mut records = std::collections::BTreeSet::new();
        for entry in fs::read_dir(directory.path()).unwrap().flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "lock")
            {
                continue;
            }
            for line in fs::read_to_string(entry.path()).unwrap().lines() {
                let record: LogRecord = serde_json::from_str(line).unwrap();
                assert!(records.insert((record.subsystem, record.event)));
            }
        }
        assert_eq!(records.len(), 200);
    }
}
