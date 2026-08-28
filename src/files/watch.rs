use super::*;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerWatchGeneration(pub u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWatchBatch {
    pub generation: ExplorerWatchGeneration,
    pub root_revision: u64,
    pub directories: Vec<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplorerWatchEvent {
    Batch(ExplorerWatchBatch),
    Failed {
        generation: ExplorerWatchGeneration,
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
        generation: ExplorerWatchGeneration,
        root_revision: u64,
        roots: Vec<PathBuf>,
        directories: Vec<PathBuf>,
    },
    Cancel,
}
pub(super) struct ExplorerWatchSet {
    generation: ExplorerWatchGeneration,
    root_revision: u64,
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    fingerprints: BTreeMap<PathBuf, ExplorerDirectoryFingerprint>,
    pending: BTreeSet<PathBuf>,
    changed_at: Option<std::time::Instant>,
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
        fs::canonicalize(directory).map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
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
    Ok(ExplorerDirectoryFingerprint {
        directory: canonical,
        entries,
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

pub struct ExplorerWatchService {
    policy: ExplorerWatchPolicy,
    sender: Option<SyncSender<ExplorerWatchCommand>>,
    receiver: Option<Receiver<ExplorerWatchEvent>>,
    generation: u64,
    reconcile_count: u64,
}
impl ExplorerWatchService {
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
                generation: 0,
                reconcile_count: 0,
            });
        }
        let (sender, commands) = mpsc::sync_channel(policy.queue_capacity);
        let (events, receiver) = mpsc::sync_channel(policy.queue_capacity);
        let worker_policy = policy.clone();
        thread::Builder::new()
            .name("dust-explorer-watch".into())
            .spawn(move || explorer_watch_worker(worker_policy, commands, events))
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        Ok(Self {
            policy,
            sender: Some(sender),
            receiver: Some(receiver),
            generation: 0,
            reconcile_count: 0,
        })
    }
    pub fn reconcile(
        &mut self,
        root_revision: u64,
        roots: Vec<PathBuf>,
        mut directories: Vec<PathBuf>,
    ) -> Result<Option<ExplorerWatchGeneration>, ExplorerWatchError> {
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
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ExplorerWatchError::Limit)?;
        let generation = ExplorerWatchGeneration(self.generation);
        self.sender
            .as_ref()
            .ok_or(ExplorerWatchError::WorkerUnavailable)?
            .try_send(ExplorerWatchCommand::Reconcile {
                generation,
                root_revision,
                roots,
                directories,
            })
            .map_err(|_| ExplorerWatchError::WorkerUnavailable)?;
        self.reconcile_count = self.reconcile_count.saturating_add(1);
        Ok(Some(generation))
    }
    pub fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(ExplorerWatchCommand::Cancel);
        }
        self.reconcile_count = self.reconcile_count.saturating_add(1);
    }
    pub fn poll(&self) -> Vec<ExplorerWatchEvent> {
        let mut events = Vec::new();
        let Some(receiver) = &self.receiver else {
            return events;
        };
        while events.len() < self.policy.maximum_events {
            match receiver.try_recv() {
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
) {
    let mut active: Option<ExplorerWatchSet> = None;
    loop {
        match commands.recv_timeout(policy.poll_interval) {
            Ok(ExplorerWatchCommand::Cancel) => active = None,
            Ok(ExplorerWatchCommand::Reconcile {
                generation,
                root_revision,
                roots,
                directories,
            }) => {
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
                        active = Some(ExplorerWatchSet {
                            generation,
                            root_revision,
                            roots,
                            directories,
                            fingerprints,
                            pending: BTreeSet::new(),
                            changed_at: None,
                        })
                    }
                    Err(_) => {
                        let _ = events.try_send(ExplorerWatchEvent::Failed {
                            generation,
                            root_revision,
                        });
                        active = None
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(set) = active.as_mut() else { continue };
                let current = collect_explorer_fingerprints(&set.directories, &set.roots, &policy);
                let Ok(current) = current else {
                    let _ = events.try_send(ExplorerWatchEvent::Failed {
                        generation: set.generation,
                        root_revision: set.root_revision,
                    });
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
                            let directories = std::mem::take(&mut set.pending)
                                .into_iter()
                                .take(policy.maximum_events)
                                .collect();
                            let _ =
                                events.try_send(ExplorerWatchEvent::Batch(ExplorerWatchBatch {
                                    generation: set.generation,
                                    root_revision: set.root_revision,
                                    directories,
                                }));
                            set.changed_at = None;
                        }
                    }
                    Err(_) => {
                        let _ = events.try_send(ExplorerWatchEvent::Failed {
                            generation: set.generation,
                            root_revision: set.root_revision,
                        });
                        active = None;
                    }
                }
            }
        }
    }
}
