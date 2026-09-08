use super::*;

pub(super) const ABSOLUTE_WORKSPACE_FILE_PATH_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFilePolicy {
    pub roots: Vec<PathBuf>,
    pub root_revision: u64,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_path_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFileEntry {
    pub id: String,
    pub canonical_path: PathBuf,
    pub relative_path: PathBuf,
    pub root: PathBuf,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceFileSummary {
    pub files: usize,
    pub path_bytes: usize,
    pub truncated: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceFileEvent {
    Batch {
        generation: u64,
        root_revision: u64,
        entries: Vec<WorkspaceFileEntry>,
    },
    Complete {
        generation: u64,
        root_revision: u64,
        summary: WorkspaceFileSummary,
    },
    Cancelled {
        generation: u64,
        root_revision: u64,
    },
    Failed {
        generation: u64,
        root_revision: u64,
        error: String,
    },
}
#[derive(Default)]
pub struct WorkspaceFileService {
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<WorkspaceFileEvent>>,
}
impl WorkspaceFileService {
    pub fn start(&mut self, policy: WorkspaceFilePolicy) -> Result<u64, WorkspaceFileError> {
        validate_workspace_file_policy(&policy)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(WorkspaceFileError::Unsafe("request identity exhausted"))?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::sync_channel(policy.event_queue_capacity);
        thread::Builder::new()
            .name(format!("dust-workspace-files-{generation}"))
            .spawn(move || run_workspace_files(generation, policy, worker, sender))
            .map_err(WorkspaceFileError::Spawn)?;
        self.cancel();
        self.generation = generation;
        self.cancellation = Some(cancellation);
        self.receiver = Some(receiver);
        Ok(generation)
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancellation.take() {
            c.store(true, Ordering::Release)
        }
        self.receiver = None
    }
    pub fn try_event(&self) -> Option<WorkspaceFileEvent> {
        self.receiver.as_ref().and_then(|r| r.try_recv().ok())
    }
    pub(crate) fn poll_event(&self) -> Result<Option<WorkspaceFileEvent>, &'static str> {
        match self.receiver.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(event)) => Ok(Some(event)),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("workspace file worker stopped before completion")
            }
            _ => Ok(None),
        }
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}
impl Drop for WorkspaceFileService {
    fn drop(&mut self) {
        self.cancel()
    }
}
#[derive(Debug, Error)]
pub enum WorkspaceFileError {
    #[error("workspace files require roots")]
    NoRoots,
    #[error("workspace file policy exceeds {0}")]
    Unsafe(&'static str),
    #[error("failed to start workspace file worker: {0}")]
    Spawn(io::Error),
}
pub(super) fn validate_workspace_file_policy(
    p: &WorkspaceFilePolicy,
) -> Result<(), WorkspaceFileError> {
    if p.roots.is_empty() {
        return Err(WorkspaceFileError::NoRoots);
    }
    if !(1..=ABSOLUTE_WORKSPACE_SEARCH_FILES).contains(&p.maximum_files) {
        return Err(WorkspaceFileError::Unsafe("maximum_files"));
    }
    if !(1..=ABSOLUTE_WORKSPACE_FILE_PATH_BYTES).contains(&p.maximum_path_bytes) {
        return Err(WorkspaceFileError::Unsafe("maximum_path_bytes"));
    }
    if !(1..=4096).contains(&p.batch_size) {
        return Err(WorkspaceFileError::Unsafe("batch_size"));
    }
    if !(1..=1024).contains(&p.event_queue_capacity) {
        return Err(WorkspaceFileError::Unsafe("event_queue_capacity"));
    }
    Ok(())
}
pub(super) fn send_file_event(
    sender: &SyncSender<WorkspaceFileEvent>,
    cancel: &AtomicBool,
    mut event: WorkspaceFileEvent,
) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(v)) => {
                event = v;
                thread::yield_now()
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}
pub(super) fn run_workspace_files(
    generation: u64,
    policy: WorkspaceFilePolicy,
    cancel: Arc<AtomicBool>,
    sender: SyncSender<WorkspaceFileEvent>,
) {
    let mut roots = policy
        .roots
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        let _ = send_file_event(
            &sender,
            &cancel,
            WorkspaceFileEvent::Failed {
                generation,
                root_revision: policy.root_revision,
                error: "no canonical workspace roots".into(),
            },
        );
        return;
    }
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in roots.iter().skip(1) {
        builder.add(root);
    }
    builder
        .follow_links(false)
        .hidden(!policy.include_hidden)
        .ignore(!policy.include_ignored)
        .git_ignore(!policy.include_ignored)
        .git_global(!policy.include_ignored)
        .git_exclude(!policy.include_ignored)
        .parents(!policy.include_ignored);
    let mut accepted = Vec::new();
    let mut seen = HashSet::new();
    let mut bytes: usize = 0;
    let mut truncated = false;
    for entry in builder.build() {
        if cancel.load(Ordering::Acquire) {
            let _ = send_file_event(
                &sender,
                &cancel,
                WorkspaceFileEvent::Cancelled {
                    generation,
                    root_revision: policy.root_revision,
                },
            );
            return;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(root) = owner_root(&path, &roots).cloned() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(&root).map(Path::to_path_buf) else {
            continue;
        };
        let cost = path
            .as_os_str()
            .len()
            .saturating_add(relative.as_os_str().len());
        if accepted.len() >= policy.maximum_files
            || bytes.saturating_add(cost) > policy.maximum_path_bytes
        {
            truncated = true;
            break;
        }
        bytes += cost;
        accepted.push(WorkspaceFileEntry {
            id: format!("{}:{}", root.display(), relative.display()),
            canonical_path: path,
            relative_path: relative,
            root,
        });
    }
    accepted.sort_by(|a, b| {
        a.root
            .cmp(&b.root)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    for batch in accepted.chunks(policy.batch_size) {
        if !send_file_event(
            &sender,
            &cancel,
            WorkspaceFileEvent::Batch {
                generation,
                root_revision: policy.root_revision,
                entries: batch.to_vec(),
            },
        ) {
            return;
        }
    }
    let _ = send_file_event(
        &sender,
        &cancel,
        WorkspaceFileEvent::Complete {
            generation,
            root_revision: policy.root_revision,
            summary: WorkspaceFileSummary {
                files: accepted.len(),
                path_bytes: bytes,
                truncated,
            },
        },
    );
}
