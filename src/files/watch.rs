//! Directory fingerprints and explorer watch changes.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::time::Duration;

use sha2::Digest;

use sha2::Sha256;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExplorerWatchEntryKind {
    Directory,
    File,
    Symlink,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExplorerWatchEntryFingerprint {
    pub name: Vec<u8>,
    pub kind: ExplorerWatchEntryKind,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerDirectoryFingerprint {
    pub directory: PathBuf,
    pub entries: Vec<ExplorerWatchEntryFingerprint>,
    pub ignore_rules: [u8; 32],
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWatchPolicy {
    pub enabled: bool,
    pub poll_interval: Duration,
    pub debounce: Duration,
    pub maximum_directories: usize,
    pub maximum_entries_per_directory: usize,
    pub maximum_events: usize,
    pub maximum_path_bytes: usize,
    pub queue_capacity: usize,
}
impl Default for ExplorerWatchPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: Duration::from_millis(500),
            debounce: Duration::from_millis(100),
            maximum_directories: 128,
            maximum_entries_per_directory: 10_000,
            maximum_events: 128,
            maximum_path_bytes: 65_536,
            queue_capacity: 8,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerWatchId(pub u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWatchBatch {
    pub stream_id: ExplorerWatchId,
    pub root_revision: u64,
    pub directories: Vec<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplorerWatchEvent {
    Batch(ExplorerWatchBatch),
    Failed {
        stream_id: ExplorerWatchId,
        root_revision: u64,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ExplorerWatchError {
    #[error("explorer watch policy is invalid")]
    InvalidPolicy,
    #[error("explorer watch set exceeds limits")]
    Limit,
    #[error("explorer watch worker is unavailable")]
    WorkerUnavailable,
}

pub(super) enum ExplorerWatchCommand {
    Reconcile {
        stream_id: ExplorerWatchId,
        root_revision: u64,
        roots: Vec<PathBuf>,
        directories: Vec<PathBuf>,
    },
}
pub(super) struct ExplorerWatchSet {
    stream_id: ExplorerWatchId,
    root_revision: u64,
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    fingerprints: BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    pending: BTreeSet<PathBuf>,
    changed_at: Option<std::time::Instant>,
}

impl ExplorerWatchSet {
    fn retain_previous(&mut self, previous: Self) {
        if self.root_revision != previous.root_revision || self.roots != previous.roots {
            return;
        }
        for (directory, fingerprint) in previous.fingerprints {
            if self.directories.contains(&directory) {
                self.pending.remove(&directory);
                self.fingerprints.insert(directory, fingerprint);
            }
        }
        self.pending.extend(
            previous
                .pending
                .into_iter()
                .filter(|directory| self.directories.contains(directory)),
        );
        if let Some(changed_at) = previous.changed_at {
            self.changed_at = Some(changed_at);
        }
        if self.pending.is_empty() {
            self.changed_at = None;
        }
    }

    fn publish(&mut self, events: &SyncSender<ExplorerWatchEvent>, maximum: usize) {
        let directories = self
            .pending
            .iter()
            .take(maximum)
            .cloned()
            .collect::<Vec<_>>();
        if events
            .try_send(ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                stream_id: self.stream_id,
                root_revision: self.root_revision,
                directories: directories.clone(),
            }))
            .is_ok()
        {
            for directory in directories {
                self.pending.remove(&directory);
            }
            if self.pending.is_empty() {
                self.changed_at = None;
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn watch_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}
#[cfg(not(unix))]
pub(super) fn watch_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    name.to_string_lossy().as_bytes().to_vec()
}

pub fn explorer_directory_fingerprint(
    directory: &Path,
    roots: &[PathBuf],
    maximum_entries: usize,
    maximum_path_bytes: usize,
) -> Result<ExplorerDirectoryFingerprint, ExplorerWatchError> {
    if maximum_entries == 0 || maximum_path_bytes == 0 {
        return Err(ExplorerWatchError::InvalidPolicy);
    }
    let metadata =
        fs::symlink_metadata(directory).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ExplorerWatchError::WorkerUnavailable);
    }
    let canonical =
        crate::files::canonicalize(directory).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(ExplorerWatchError::Limit);
    }
    let mut entries = Vec::new();
    let mut bytes = canonical.as_os_str().len();
    let read = fs::read_dir(&canonical).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
    for entry in read {
        let entry = entry.map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        if entries.len() >= maximum_entries {
            return Err(ExplorerWatchError::Limit);
        }
        let name = watch_name_bytes(&entry.file_name());
        bytes = bytes.saturating_add(name.len());
        if bytes > maximum_path_bytes {
            return Err(ExplorerWatchError::Limit);
        }
        let kind = entry
            .file_type()
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        entries.push(ExplorerWatchEntryFingerprint {
            name,
            kind: if kind.is_dir() {
                ExplorerWatchEntryKind::Directory
            } else if kind.is_file() {
                ExplorerWatchEntryKind::File
            } else {
                ExplorerWatchEntryKind::Symlink
            },
        });
    }
    entries.sort();
    let mut ignore_rules = Sha256::new();
    // Use the same global ignore source as WalkBuilder's directory listings.
    let ignore_paths = canonical
        .ancestors()
        .flat_map(|ancestor| {
            [".gitignore", ".ignore", ".git/info/exclude"].map(|name| ancestor.join(name))
        })
        .chain(ignore::gitignore::gitconfig_excludes_path());
    for path in ignore_paths {
        // Ignore files may be symlinks; the bounded reader still checks that
        // the resolved target is a regular file and opens it without following links.
        let contents = crate::files::canonicalize(&path)
            .map_err(|source| crate::files::VfsError::Io {
                operation: "resolve ignore file",
                path: path.clone(),
                source,
            })
            .and_then(|target| crate::files::vfs::read_file_bounded(&target, 512 * 1024));
        match contents {
            Ok(bytes) => {
                ignore_rules.update(watch_name_bytes(path.as_os_str()));
                ignore_rules.update((bytes.len() as u64).to_le_bytes());
                ignore_rules.update(bytes);
            }
            Err(crate::files::VfsError::Io { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return Err(ExplorerWatchError::WorkerUnavailable),
        }
    }
    Ok(ExplorerDirectoryFingerprint {
        directory: canonical,
        entries,
        ignore_rules: ignore_rules.finalize().into(),
    })
}

pub(super) fn collect_explorer_fingerprints(
    directories: &[PathBuf],
    roots: &[PathBuf],
    policy: &ExplorerWatchPolicy,
) -> Result<BTreeMap<PathBuf, ExplorerDirectoryFingerprint>, ExplorerWatchError> {
    let mut snapshots = BTreeMap::new();
    for directory in directories {
        match explorer_directory_fingerprint(
            directory,
            roots,
            policy.maximum_entries_per_directory,
            policy.maximum_path_bytes,
        ) {
            Ok(fingerprint) => {
                snapshots.insert(directory.clone(), fingerprint);
            }
            Err(ExplorerWatchError::WorkerUnavailable) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(snapshots)
}

pub fn diff_explorer_fingerprints(
    previous: &BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    current: &BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    maximum_events: usize,
) -> Result<Vec<PathBuf>, ExplorerWatchError> {
    let mut paths = previous
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths.retain(|path| previous.get(path) != current.get(path));
    if paths.len() > maximum_events {
        return Err(ExplorerWatchError::Limit);
    }
    Ok(paths.into_iter().collect())
}

pub struct ExplorerWatchWorker {
    policy: ExplorerWatchPolicy,
    sender: Option<SyncSender<ExplorerWatchCommand>>,
    receiver: Option<Receiver<ExplorerWatchEvent>>,
    stream_id: u64,
    reconcile_count: u64,
    active: Option<(ExplorerWatchId, u64, Vec<PathBuf>)>,
    cancelled_through: Arc<AtomicU64>,
}
impl ExplorerWatchWorker {
    pub fn start(policy: ExplorerWatchPolicy) -> Result<Self, ExplorerWatchError> {
        if policy.maximum_directories == 0
            || policy.maximum_entries_per_directory == 0
            || policy.maximum_events == 0
            || policy.maximum_path_bytes == 0
            || policy.queue_capacity == 0
            || policy.poll_interval.is_zero()
        {
            return Err(ExplorerWatchError::InvalidPolicy);
        }
        if !policy.enabled {
            return Ok(Self {
                policy,
                sender: None,
                receiver: None,
                stream_id: 0,
                reconcile_count: 0,
                active: None,
                cancelled_through: Arc::new(AtomicU64::new(0)),
            });
        }
        let (sender, commands) = mpsc::sync_channel(policy.queue_capacity);
        let (events, receiver) = mpsc::sync_channel(policy.queue_capacity);
        let worker_policy = policy.clone();
        let cancelled_through = Arc::new(AtomicU64::new(0));
        let worker_cancelled = Arc::clone(&cancelled_through);
        thread::Builder::new()
            .name("dust-explorer-watch".into())
            .spawn(move || explorer_watch_worker(worker_policy, commands, events, worker_cancelled))
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        Ok(Self {
            policy,
            sender: Some(sender),
            receiver: Some(receiver),
            stream_id: 0,
            reconcile_count: 0,
            active: None,
            cancelled_through,
        })
    }
    pub fn reconcile(
        &mut self,
        root_revision: u64,
        roots: Vec<PathBuf>,
        mut directories: Vec<PathBuf>,
    ) -> Result<Option<ExplorerWatchId>, ExplorerWatchError> {
        if !self.policy.enabled {
            return Ok(None);
        }
        directories.sort();
        directories.dedup();
        if directories.len() > self.policy.maximum_directories
            || directories
                .iter()
                .map(|path| path.as_os_str().len())
                .sum::<usize>()
                > self.policy.maximum_path_bytes
        {
            return Err(ExplorerWatchError::Limit);
        }
        self.stream_id = self
            .stream_id
            .checked_add(1)
            .ok_or(ExplorerWatchError::Limit)?;
        let stream_id = ExplorerWatchId(self.stream_id);
        self.sender
            .as_ref()
            .ok_or(ExplorerWatchError::WorkerUnavailable)?
            .try_send(ExplorerWatchCommand::Reconcile {
                stream_id,
                root_revision,
                roots,
                directories: directories.clone(),
            })
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        self.active = Some((stream_id, root_revision, directories));
        self.reconcile_count = self.reconcile_count.saturating_add(1);
        Ok(Some(stream_id))
    }
    pub fn cancel(&mut self) {
        self.stream_id = self.stream_id.saturating_add(1);
        self.cancelled_through
            .store(self.stream_id, Ordering::Relaxed);
        self.active = None;
        self.reconcile_count = self.reconcile_count.saturating_add(1);
    }
    pub fn poll(&self) -> Vec<ExplorerWatchEvent> {
        let mut events = Vec::new();
        let Some(receiver) = &self.receiver else {
            return events;
        };
        while events.len() < self.policy.maximum_events {
            match receiver.try_recv() {
                Ok(ExplorerWatchEvent::Batch(mut batch)) => {
                    let Some((stream_id, root_revision, directories)) = &self.active else {
                        continue;
                    };
                    if batch.root_revision != *root_revision
                        || batch.stream_id.0 <= self.cancelled_through.load(Ordering::Relaxed)
                    {
                        continue;
                    }
                    batch.directories.retain(|path| directories.contains(path));
                    if !batch.directories.is_empty() {
                        // Queued changes remain valid for directories retained by
                        // a newer reconciliation, even after their old set published them.
                        batch.stream_id = *stream_id;
                        events.push(ExplorerWatchEvent::Batch(batch));
                    }
                }
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        events
    }
    #[must_use]
    pub const fn reconcile_count(&self) -> u64 {
        self.reconcile_count
    }
}

pub(super) fn explorer_watch_worker(
    policy: ExplorerWatchPolicy,
    commands: Receiver<ExplorerWatchCommand>,
    events: SyncSender<ExplorerWatchEvent>,
    cancelled_through: Arc<AtomicU64>,
) {
    let mut active: Option<ExplorerWatchSet> = None;
    let mut failure: Option<(ExplorerWatchId, u64)> = None;
    loop {
        let cancelled = cancelled_through.load(Ordering::Relaxed);
        if active
            .as_ref()
            .is_some_and(|set| set.stream_id.0 <= cancelled)
        {
            active = None;
        }
        if let Some((stream_id, root_revision)) = failure {
            if stream_id.0 <= cancelled {
                failure = None;
            } else {
                match events.try_send(ExplorerWatchEvent::Failed {
                    stream_id,
                    root_revision,
                }) {
                    Ok(()) => failure = None,
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        }
        match commands.recv_timeout(policy.poll_interval) {
            Ok(ExplorerWatchCommand::Reconcile {
                stream_id,
                root_revision,
                roots,
                directories,
            }) => {
                if stream_id.0 <= cancelled_through.load(Ordering::Relaxed) {
                    continue;
                }
                failure = None;
                let fingerprints = directories
                    .iter()
                    .map(|directory| {
                        explorer_directory_fingerprint(
                            directory,
                            &roots,
                            policy.maximum_entries_per_directory,
                            policy.maximum_path_bytes,
                        )
                        .map(|fingerprint| (directory.clone(), fingerprint))
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>();
                match fingerprints {
                    Ok(fingerprints) => {
                        let mut next = ExplorerWatchSet {
                            stream_id,
                            root_revision,
                            roots,
                            // Refresh new watches after their baseline is established:
                            // the displayed listing may predate this fingerprint.
                            pending: directories.iter().cloned().collect(),
                            changed_at: Some(std::time::Instant::now()),
                            directories,
                            fingerprints,
                        };
                        if let Some(previous) = active.take() {
                            next.retain_previous(previous);
                        }
                        active = Some(next);
                    }
                    Err(_) => {
                        failure = Some((stream_id, root_revision));
                        active = None
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(set) = active.as_mut() else { continue };
                if set.stream_id.0 <= cancelled_through.load(Ordering::Relaxed) {
                    active = None;
                    continue;
                }
                let current = collect_explorer_fingerprints(&set.directories, &set.roots, &policy);
                let Ok(current) = current else {
                    failure = Some((set.stream_id, set.root_revision));
                    active = None;
                    continue;
                };
                match diff_explorer_fingerprints(&set.fingerprints, &current, policy.maximum_events)
                {
                    Ok(changed) => {
                        set.fingerprints = current;
                        if !changed.is_empty() {
                            set.pending.extend(changed);
                            set.changed_at.get_or_insert_with(std::time::Instant::now);
                        }
                        if !set.pending.is_empty()
                            && set
                                .changed_at
                                .is_some_and(|at| at.elapsed() >= policy.debounce)
                        {
                            set.publish(&events, policy.maximum_events);
                        }
                    }
                    Err(_) => {
                        failure = Some((set.stream_id, set.root_revision));
                        active = None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::files::watch::*;

    fn watch_set(stream_id: u64, directories: Vec<PathBuf>) -> ExplorerWatchSet {
        ExplorerWatchSet {
            stream_id: ExplorerWatchId(stream_id),
            root_revision: 1,
            roots: directories.clone(),
            directories,
            fingerprints: BTreeMap::new(),
            pending: BTreeSet::new(),
            changed_at: None,
        }
    }

    fn service_channels() -> (
        ExplorerWatchWorker,
        Receiver<ExplorerWatchCommand>,
        SyncSender<ExplorerWatchEvent>,
    ) {
        let (sender, commands) = mpsc::sync_channel(2);
        let (events, receiver) = mpsc::sync_channel(2);
        (
            ExplorerWatchWorker {
                policy: ExplorerWatchPolicy::default(),
                sender: Some(sender),
                receiver: Some(receiver),
                stream_id: 0,
                reconcile_count: 0,
                active: None,
                cancelled_through: Arc::new(AtomicU64::new(0)),
            },
            commands,
            events,
        )
    }

    #[test]
    fn queued_changes_survive_reconciliation_only_for_retained_directories() {
        let (mut service, _commands, events) = service_channels();
        let a = PathBuf::from("a");
        let b = PathBuf::from("b");
        let first = service
            .reconcile(1, vec![], vec![a.clone(), b.clone()])
            .unwrap()
            .unwrap();
        events
            .send(ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                stream_id: first,
                root_revision: 1,
                directories: vec![a.clone(), b],
            }))
            .unwrap();
        let second = service
            .reconcile(1, vec![], vec![a.clone()])
            .unwrap()
            .unwrap();
        assert_eq!(
            service.poll(),
            vec![ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                stream_id: second,
                root_revision: 1,
                directories: vec![a],
            })]
        );
    }

    #[test]
    fn cancellation_is_observable_even_with_a_full_command_queue() {
        let (mut service, commands, events) = service_channels();
        // Both queued sets are invalid, so either being processed would fail.
        for _ in 0..2 {
            service
                .reconcile(1, vec![], vec![PathBuf::from("missing")])
                .unwrap();
        }
        service.cancel();
        let cancelled = Arc::clone(&service.cancelled_through);
        assert_eq!(cancelled.load(Ordering::Relaxed), 3);
        let worker = thread::spawn(move || {
            explorer_watch_worker(
                ExplorerWatchPolicy {
                    poll_interval: Duration::from_millis(10),
                    ..ExplorerWatchPolicy::default()
                },
                commands,
                events,
                cancelled,
            )
        });
        // A later invalid set still reports failure; neither cancelled set may do so.
        service
            .sender
            .as_ref()
            .unwrap()
            .send(ExplorerWatchCommand::Reconcile {
                stream_id: ExplorerWatchId(4),
                root_revision: 1,
                roots: vec![],
                directories: vec![PathBuf::from("missing")],
            })
            .unwrap();
        assert_eq!(
            service
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            ExplorerWatchEvent::Failed {
                stream_id: ExplorerWatchId(4),
                root_revision: 1
            }
        );
        drop(service);
        worker.join().unwrap();
    }

    #[test]
    fn watch_failures_are_retried_when_the_event_queue_is_full() {
        let (sender, commands) = mpsc::sync_channel(1);
        let (events, receiver) = mpsc::sync_channel(1);
        let stale = ExplorerWatchEvent::Failed {
            stream_id: ExplorerWatchId(1),
            root_revision: 1,
        };
        events.send(stale.clone()).unwrap();
        let worker = thread::spawn(move || {
            explorer_watch_worker(
                ExplorerWatchPolicy {
                    poll_interval: Duration::from_millis(10),
                    ..ExplorerWatchPolicy::default()
                },
                commands,
                events,
                Arc::new(AtomicU64::new(0)),
            )
        });
        sender
            .send(ExplorerWatchCommand::Reconcile {
                stream_id: ExplorerWatchId(2),
                root_revision: 1,
                roots: vec![],
                directories: vec![PathBuf::from("missing")],
            })
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(receiver.recv().unwrap(), stale);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            ExplorerWatchEvent::Failed {
                stream_id: ExplorerWatchId(2),
                root_revision: 1
            }
        );
        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn global_ignore_changes_invalidate_fingerprints() {
        const CHILD_ROOT: &str = "DUST_TEST_GLOBAL_IGNORE_ROOT";
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            let root = tempfile::tempdir().unwrap();
            let excludes = root.path().join("excludes");
            let config = root.path().join("gitconfig");
            // Forward slashes need no escaping in git config, on Windows too.
            let excludes = excludes.display().to_string().replace('\\', "/");
            fs::write(&config, format!("[core]\nexcludesFile = \"{excludes}\"\n")).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "files::watch::tests::global_ignore_changes_invalidate_fingerprints",
                    "--nocapture",
                ])
                .env(CHILD_ROOT, root.path())
                .env("HOME", root.path())
                .env("XDG_CONFIG_HOME", root.path())
                .env("GIT_CONFIG_GLOBAL", config)
                .env("GIT_CONFIG_SYSTEM", root.path().join("no-system-config"))
                .status()
                .unwrap();
            assert!(status.success());
            return;
        };
        let root = PathBuf::from(root);
        let directory = root.join("workspace");
        fs::create_dir(&directory).unwrap();
        let excludes = root.join("excludes");
        assert_eq!(
            ignore::gitignore::gitconfig_excludes_path(),
            Some(excludes.clone())
        );
        let roots = vec![crate::files::canonicalize(&directory).unwrap()];
        fs::write(&excludes, "one.txt\n").unwrap();
        let before = explorer_directory_fingerprint(&directory, &roots, 10, 4096).unwrap();
        fs::write(&excludes, "two.txt\n").unwrap();
        let after = explorer_directory_fingerprint(&directory, &roots, 10, 4096).unwrap();
        assert_eq!(before.entries, after.entries);
        assert_ne!(before.ignore_rules, after.ignore_rules);
        // Changing the configured source is observed even with no directory changes.
        let replacement = root.join("replacement");
        fs::write(&replacement, "three.txt\n").unwrap();
        fs::write(
            root.join("gitconfig"),
            format!("[core]\nexcludesFile = {:?}\n", replacement),
        )
        .unwrap();
        assert_ne!(
            after.ignore_rules,
            explorer_directory_fingerprint(&directory, &roots, 10, 4096)
                .unwrap()
                .ignore_rules
        );
    }

    #[test]
    fn newly_watched_directories_are_invalidated_after_the_initial_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let root_path = crate::files::canonicalize(root.path()).unwrap();
        // The displayed listing was empty; the file appears before watching starts.
        fs::write(root.path().join("new"), "new").unwrap();
        let mut service = ExplorerWatchWorker::start(ExplorerWatchPolicy {
            poll_interval: Duration::from_millis(10),
            debounce: Duration::ZERO,
            ..ExplorerWatchPolicy::default()
        })
        .unwrap();
        let stream_id = service
            .reconcile(1, vec![root_path.clone()], vec![root_path.clone()])
            .unwrap()
            .unwrap();
        assert_eq!(
            service
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                stream_id,
                root_revision: 1,
                directories: vec![root_path.clone()],
            })
        );
        // Retaining an unchanged watch must not trigger another initial refresh.
        service
            .reconcile(1, vec![root_path.clone()], vec![root_path])
            .unwrap();
        assert!(matches!(
            service
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn reconciliation_preserves_retained_fingerprints_and_pending_changes() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("a");
        let added = root.path().join("b");
        fs::create_dir(&directory).unwrap();
        fs::create_dir(&added).unwrap();
        let roots = vec![root.path().to_path_buf()];
        let before = explorer_directory_fingerprint(&directory, &roots, 10, 4096).unwrap();
        fs::write(directory.join("new"), "new").unwrap();
        let after = explorer_directory_fingerprint(&directory, &roots, 10, 4096).unwrap();
        let mut previous = watch_set(1, vec![directory.clone()]);
        previous.roots = roots.clone();
        previous
            .fingerprints
            .insert(directory.clone(), before.clone());
        previous.pending.insert(directory.clone());
        previous.changed_at = Some(std::time::Instant::now());
        let mut next = watch_set(2, vec![directory.clone(), added]);
        next.roots = roots;
        next.fingerprints.insert(directory.clone(), after);
        next.retain_previous(previous);
        assert_eq!(next.fingerprints[&directory], before);
        assert!(next.changed_at.is_some());
        let (send, receive) = mpsc::sync_channel(1);
        next.publish(&send, 1);
        assert_eq!(
            receive.recv().unwrap(),
            ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                stream_id: ExplorerWatchId(2),
                root_revision: 1,
                directories: vec![directory],
            })
        );
    }

    #[test]
    fn full_event_queue_retries_pending_directory_batches() {
        let a = PathBuf::from("a");
        let b = PathBuf::from("b");
        let mut set = watch_set(2, vec![a.clone(), b.clone()]);
        set.pending.extend([a.clone(), b.clone()]);
        set.changed_at = Some(std::time::Instant::now());
        let (send, receive) = mpsc::sync_channel(1);
        send.try_send(ExplorerWatchEvent::Failed {
            stream_id: ExplorerWatchId(1),
            root_revision: 1,
        })
        .unwrap();
        set.publish(&send, 1);
        assert_eq!(set.pending.len(), 2);
        assert!(set.changed_at.is_some());
        receive.recv().unwrap();
        for directory in [a, b] {
            set.publish(&send, 1);
            assert!(
                matches!(receive.recv().unwrap(), ExplorerWatchEvent::Batch(batch)
                if batch.directories == vec![directory])
            );
        }
        assert!(set.pending.is_empty());
        assert!(set.changed_at.is_none());
    }
}
