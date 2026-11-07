//! Repository status reads and accepted snapshots.

use std::collections::BTreeMap;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitStatusRequestId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitRepositoryId(pub u64);
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitFileStatus {
    Untracked,
    Added,
    Modified,
    TypeChanged,
    Deleted,
    Renamed,
    Conflicted,
}
/// What a change set is relative to. A caller never has to infer this: the reader that
/// produced the data states it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitChangeSource {
    /// HEAD to index: what a commit would record.
    Staged,
    /// Index to disk: changed but not yet staged.
    Unstaged,
    /// Unsaved editor content relative to the index, at one exact document revision.
    Unsaved { document_revision: u64 },
}
/// Which sources currently differ for one path. A false bit is not a claim that the source
/// matches; it means the reader attributed no change to it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitChangeSources {
    pub staged: bool,
    pub unstaged: bool,
    pub unsaved_document_revision: Option<u64>,
}
impl GitChangeSource {
    /// Plain-text label for this source, for a picker row's secondary text.
    #[must_use]
    pub const fn source_label(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Unsaved { .. } => "unsaved",
        }
    }
}
impl GitChangeSources {
    /// Plain-text label naming every source that currently differs for one path. Returned as a
    /// `&'static str` so composing a bounded row allocates nothing, and so a path that is both
    /// staged and further modified is never described as only one of them.
    #[must_use]
    pub const fn source_label(&self) -> &'static str {
        match (
            self.staged,
            self.unstaged,
            self.unsaved_document_revision.is_some(),
        ) {
            (true, true, true) => "staged+unstaged+unsaved",
            (true, true, false) => "staged+unstaged",
            (true, false, true) => "staged+unsaved",
            (false, true, true) => "unstaged+unsaved",
            (true, false, false) => "staged",
            (false, true, false) => "unstaged",
            (false, false, true) => "unsaved",
            (false, false, false) => "unchanged",
        }
    }
    #[must_use]
    pub const fn contains(&self, source: GitChangeSource) -> bool {
        match source {
            GitChangeSource::Staged => self.staged,
            GitChangeSource::Unstaged => self.unstaged,
            GitChangeSource::Unsaved { document_revision } => {
                matches!(self.unsaved_document_revision, Some(held) if held == document_revision)
            }
        }
    }
    #[must_use]
    pub const fn count(&self) -> u8 {
        self.staged as u8 + self.unstaged as u8 + self.unsaved_document_revision.is_some() as u8
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count() == 0
    }
}
/// Unsaved editor content offered to a Git reader as data. The Git owner holds no editor
/// state; a caller supplies the bounded list and the reader only attributes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitUnsavedDocument {
    pub relative_path: PathBuf,
    pub document_revision: u64,
}
impl GitUnsavedDocument {
    #[must_use]
    pub fn new(relative_path: PathBuf, document_revision: u64) -> Self {
        Self {
            relative_path,
            document_revision,
        }
    }
}
/// Resolve the unsaved revision for one workspace-relative path. The list is expected to be
/// sorted and duplicate-free by the validator; binary search keeps lookup logarithmic.
#[must_use]
pub fn unsaved_document_revision(
    documents: &[GitUnsavedDocument],
    relative_path: &Path,
) -> Option<u64> {
    documents
        .binary_search_by(|document| document.relative_path.as_path().cmp(relative_path))
        .ok()
        .map(|index| documents[index].document_revision)
}
/// Reject an unsaved list that is unsorted, duplicated, revision-zero, or beyond the
/// status policy. The list is bounded by the
/// same file and path-byte limits the caller already accepted.
pub fn validate_unsaved_documents(
    documents: &[GitUnsavedDocument],
    maximum_files: usize,
    maximum_path_bytes: usize,
) -> Result<(), GitStatusError> {
    if documents.len() > maximum_files || documents.len() > 100_000 {
        return Err(GitStatusError::InvalidPolicy);
    }
    let mut bytes = 0usize;
    let mut previous: Option<&Path> = None;
    for document in documents {
        if document.document_revision == 0
            || document.relative_path.is_absolute()
            || document
                .relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(GitStatusError::InvalidPolicy);
        }
        if previous.is_some_and(|last| last >= document.relative_path.as_path()) {
            return Err(GitStatusError::InvalidPolicy);
        }
        bytes = bytes.saturating_add(document.relative_path.as_os_str().len());
        previous = Some(document.relative_path.as_path());
    }
    if bytes > maximum_path_bytes {
        return Err(GitStatusError::InvalidPolicy);
    }
    Ok(())
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GitStatusEntry {
    pub relative_path: PathBuf,
    /// Repository-relative source of a rename, including sources outside a nested scope.
    pub original_path: Option<PathBuf>,
    pub status: GitFileStatus,
    pub staged_status: Option<GitFileStatus>,
    pub unstaged_status: Option<GitFileStatus>,
    pub sources: GitChangeSources,
}
impl GitStatusEntry {
    #[must_use]
    pub fn change_code(&self) -> String {
        if self.status == GitFileStatus::Conflicted {
            return "UU".into();
        }
        if self.status == GitFileStatus::Untracked {
            return "??".into();
        }
        let letter = |status: &Option<GitFileStatus>| match status {
            Some(GitFileStatus::Added) => 'A',
            Some(GitFileStatus::Modified) => 'M',
            Some(GitFileStatus::TypeChanged) => 'T',
            Some(GitFileStatus::Deleted) => 'D',
            Some(GitFileStatus::Renamed) => 'R',
            Some(GitFileStatus::Conflicted) => 'U',
            Some(GitFileStatus::Untracked) => '?',
            None => ' ',
        };
        format!(
            "{}{}",
            letter(&self.staged_status),
            letter(&self.unstaged_status)
        )
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusSnapshot {
    pub request_id: GitStatusRequestId,
    pub root_revision: u64,
    pub repository: PathBuf,
    /// The folder holding this checkout's index and HEAD.
    pub git_dir: PathBuf,
    /// The parent repository's name when the scope is a linked worktree.
    pub worktree_repository: Option<String>,
    pub scope_root: PathBuf,
    pub repository_id: GitRepositoryId,
    pub entries: Vec<GitStatusEntry>,
    pub truncated: bool,
    pub revision: u64,
}
impl GitStatusSnapshot {
    /// Update editor-only changes without re-reading or changing the disk status revision.
    pub(crate) fn reconcile_unsaved_documents(
        &mut self,
        documents: &[GitUnsavedDocument],
        maximum_files: usize,
        maximum_path_bytes: usize,
    ) {
        self.entries
            .retain(|entry| entry.sources.staged || entry.sources.unstaged);
        for entry in &mut self.entries {
            entry.sources.unsaved_document_revision = None;
        }
        let mut bytes: usize = self
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_os_str().len())
            .sum();
        for document in documents {
            let position = match self.entries.binary_search_by(|entry| {
                entry.relative_path.as_path().cmp(&document.relative_path)
            }) {
                Ok(position) => {
                    self.entries[position].sources.unsaved_document_revision =
                        Some(document.document_revision);
                    continue;
                }
                Err(position) => position,
            };
            let cost = document.relative_path.as_os_str().len();
            if self.entries.len() >= maximum_files
                || bytes.saturating_add(cost) > maximum_path_bytes
            {
                self.truncated = true;
                continue;
            }
            bytes += cost;
            self.entries.insert(
                position,
                GitStatusEntry {
                    relative_path: document.relative_path.clone(),
                    original_path: None,
                    status: GitFileStatus::Modified,
                    staged_status: None,
                    unstaged_status: None,
                    sources: GitChangeSources {
                        staged: false,
                        unstaged: false,
                        unsaved_document_revision: Some(document.document_revision),
                    },
                },
            );
        }
    }
}
#[derive(Clone, Debug)]
pub struct GitStatusPolicy {
    pub root: PathBuf,
    pub root_revision: u64,
    pub maximum_files: usize,
    pub maximum_path_bytes: usize,
    pub deadline: Duration,
}
#[derive(Debug, Error)]
pub enum GitStatusError {
    #[error("invalid git status policy")]
    InvalidPolicy,
    #[error("not a Git repository")]
    NotRepository,
    #[error("Git repository is outside workspace scope")]
    OutsideScope,
    #[error("bare Git repositories are unsupported")]
    BareUnsupported,
    #[error("Git status deadline exceeded")]
    Deadline,
    #[error("Git status was cancelled")]
    Cancelled,
    #[error("Git status worker stopped")]
    Stopped,
    #[error("Git backend failed: {0}")]
    Backend(String),
}
#[derive(Debug)]
pub(super) enum Event {
    Complete(Result<GitStatusSnapshot, GitStatusError>),
}
#[derive(Default)]
pub struct GitStatusWorker {
    next: u64,
    cancel: Option<Arc<AtomicBool>>,
    receiver: Option<Receiver<Event>>,
    accepted: Option<GitStatusSnapshot>,
    active_request_id: Option<GitStatusRequestId>,
    revision: u64,
    worker: Option<thread::JoinHandle<()>>,
    pending: Option<(GitStatusRequestId, GitStatusPolicy, Vec<GitUnsavedDocument>)>,
}
impl GitStatusWorker {
    pub fn start(&mut self, policy: GitStatusPolicy) -> Result<GitStatusRequestId, GitStatusError> {
        self.start_with_unsaved(policy, Vec::new())
    }

    /// Start a status read that also attributes unsaved editor content. The list is bounded
    /// by the policy's own file and path-byte limits and must be sorted by path.
    pub fn start_with_unsaved(
        &mut self,
        policy: GitStatusPolicy,
        unsaved: Vec<GitUnsavedDocument>,
    ) -> Result<GitStatusRequestId, GitStatusError> {
        if policy.maximum_files == 0
            || policy.maximum_files > 100_000
            || policy.maximum_path_bytes == 0
            || policy.maximum_path_bytes > 16 * 1024 * 1024
            || policy.deadline.is_zero()
            || policy.deadline > Duration::from_secs(30)
        {
            return Err(GitStatusError::InvalidPolicy);
        }
        validate_unsaved_documents(&unsaved, policy.maximum_files, policy.maximum_path_bytes)?;
        self.cancel();
        self.next = self.next.saturating_add(1);
        let request_id = GitStatusRequestId(self.next);
        self.active_request_id = Some(request_id);
        self.pending = Some((request_id, policy, unsaved));
        self.launch_pending()?;
        Ok(request_id)
    }
    fn launch_pending(&mut self) -> Result<(), GitStatusError> {
        // Wait off the UI thread for cancellation to finish before launching another scan.
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let Some((request_id, policy, unsaved)) = self.pending.take() else {
            return Ok(());
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        self.worker = Some(
            thread::Builder::new()
                .name(format!("dust-git-status-{}", request_id.0))
                .spawn(move || {
                    let result = read_status(request_id, &policy, &unsaved, &worker);
                    let _ = tx.send(Event::Complete(result));
                })
                .map_err(|_| GitStatusError::Stopped)?,
        );
        self.cancel = Some(cancel);
        self.receiver = Some(rx);
        Ok(())
    }
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Release)
        }
        self.receiver = None;
        self.pending = None;
        self.active_request_id = None
    }
    pub fn poll(&mut self) -> Result<Option<&GitStatusSnapshot>, GitStatusError> {
        self.launch_pending()?;
        let Some(rx) = &self.receiver else {
            return Ok(None);
        };
        match rx.try_recv() {
            Ok(Event::Complete(result)) => {
                let mut snapshot = result?;
                if self.active_request_id != Some(snapshot.request_id) {
                    return Ok(None);
                }
                let changed = self.accepted.as_ref().is_none_or(|old| {
                    old.repository != snapshot.repository
                        || old.scope_root != snapshot.scope_root
                        || old.entries != snapshot.entries
                        || old.truncated != snapshot.truncated
                        || old.root_revision != snapshot.root_revision
                });
                if changed {
                    self.revision = self.revision.saturating_add(1)
                }
                snapshot.revision = self.revision;
                self.accepted = Some(snapshot);
                self.receiver = None;
                self.active_request_id = None;
                Ok(self.accepted.as_ref())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(GitStatusError::Stopped),
        }
    }
    pub const fn request_id(&self) -> u64 {
        self.next
    }
    pub fn snapshot_if_changed(&self, last: u64) -> Option<&GitStatusSnapshot> {
        self.accepted.as_ref().filter(|s| s.revision != last)
    }
}
impl Drop for GitStatusWorker {
    fn drop(&mut self) {
        self.cancel()
    }
}
pub(super) fn backend(message: &str) -> GitStatusError {
    GitStatusError::Backend(message.chars().take(1024).collect())
}
pub(super) fn git_status_precedence(status: &GitFileStatus) -> u8 {
    match status {
        GitFileStatus::Untracked => 0,
        GitFileStatus::Added => 1,
        GitFileStatus::Modified | GitFileStatus::TypeChanged => 2,
        GitFileStatus::Renamed => 3,
        GitFileStatus::Deleted => 4,
        GitFileStatus::Conflicted => 5,
    }
}

/// Build immutable file and ancestor-directory decorations. Conflicted wins,
/// followed by deleted, renamed, modified, added, and untracked.
#[must_use]
pub fn aggregate_git_status(
    scope_root: &Path,
    entries: &[GitStatusEntry],
) -> std::collections::BTreeMap<PathBuf, GitFileStatus> {
    let mut result = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.relative_path.is_absolute()
            || entry
                .relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            continue;
        }
        let absolute = scope_root.join(&entry.relative_path);
        if !absolute.starts_with(scope_root) {
            continue;
        }
        let mut current = Some(absolute.as_path());
        while let Some(path) = current {
            if !path.starts_with(scope_root) {
                break;
            }
            let replace = result.get(path).is_none_or(|existing| {
                git_status_precedence(&entry.status) > git_status_precedence(existing)
            });
            if replace {
                result.insert(path.to_path_buf(), entry.status.clone());
            }
            if path == scope_root {
                break;
            }
            current = path.parent();
        }
    }
    result
}

fn check_status_deadline(
    policy: &GitStatusPolicy,
    started: Instant,
    cancel: &AtomicBool,
) -> Result<(), GitStatusError> {
    if cancel.load(Ordering::Acquire) {
        Err(GitStatusError::Cancelled)
    } else if started.elapsed() >= policy.deadline {
        Err(GitStatusError::Deadline)
    } else {
        Ok(())
    }
}

/// Run Git under the existing job supervisor so a stalled scan can be killed and reaped.
pub(super) fn status_command(
    workdir: &Path,
    arguments: &[&str],
    policy: &GitStatusPolicy,
    started: Instant,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, bool), GitStatusError> {
    use crate::process::JobEvent;
    use crate::process::JobPolicy;
    use crate::process::JobRequest;
    use crate::process::JobStream;
    use crate::process::JobSupervisor;
    check_status_deadline(policy, started, cancel)?;
    let program = crate::tools::discover_executable("git")
        .ok_or_else(|| backend("git executable not found"))?;
    let maximum_output_bytes = policy
        .maximum_path_bytes
        .saturating_mul(2)
        .saturating_add(policy.maximum_files.saturating_mul(128))
        .clamp(1024, 64 * 1024 * 1024);
    let mut environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
        .collect::<BTreeMap<_, _>>();
    // Do not let inherited Git directory overrides retarget this repository.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        environment.remove(key);
    }
    environment.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
    let mut jobs = JobSupervisor::new(16);
    let job = jobs
        .start(JobRequest {
            program,
            argv0: None,
            arguments: ["-c", "core.fsmonitor=false", "-c", "color.ui=false"]
                .into_iter()
                .chain(arguments.iter().copied())
                .map(Into::into)
                .collect(),
            cwd: workdir.to_path_buf(),
            environment,
            root_revision: policy.root_revision,
            stdin: None,
            policy: JobPolicy {
                maximum_output_bytes,
                maximum_line_bytes: maximum_output_bytes.min(16 * 1024 * 1024),
                timeout: policy
                    .deadline
                    .saturating_sub(started.elapsed())
                    .max(Duration::from_nanos(1)),
                ..JobPolicy::default()
            },
        })
        .map_err(|error| backend(&error.to_string()))?;
    let mut output = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut failure = None;
    loop {
        if failure.is_none() {
            failure = check_status_deadline(policy, started, cancel).err();
            if failure.is_some() {
                let _ = jobs.cancel(job);
            }
        }
        for event in jobs.poll() {
            match event {
                JobEvent::Output {
                    stream: JobStream::Stdout,
                    bytes,
                    truncated: clipped,
                    ..
                } => {
                    if !truncated {
                        output.extend(bytes);
                    }
                    if clipped && !truncated {
                        truncated = true;
                        let _ = jobs.cancel(job);
                    }
                }
                JobEvent::Output {
                    stream: JobStream::Stderr,
                    bytes,
                    ..
                } => {
                    stderr.extend(
                        bytes
                            .into_iter()
                            .take(1024usize.saturating_sub(stderr.len())),
                    );
                }
                JobEvent::Exited {
                    code, timed_out, ..
                } => {
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    if timed_out {
                        return Err(GitStatusError::Deadline);
                    }
                    if code != Some(0) && !truncated {
                        return Err(backend(&String::from_utf8_lossy(&stderr)));
                    }
                    return Ok((output, truncated));
                }
                JobEvent::Failed { message, .. } => {
                    return Err(failure.unwrap_or_else(|| backend(&message)));
                }
                JobEvent::Started { .. } => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}

pub(super) fn read_status(
    request_id: GitStatusRequestId,
    p: &GitStatusPolicy,
    unsaved: &[GitUnsavedDocument],
    cancel: &AtomicBool,
) -> Result<GitStatusSnapshot, GitStatusError> {
    let root = crate::files::canonicalize(&p.root).map_err(|e| backend(&e.to_string()))?;
    let repo = git2::Repository::discover(&root).map_err(|_| GitStatusError::NotRepository)?;
    let workdir = repo.workdir().ok_or(GitStatusError::BareUnsupported)?;
    let workdir = crate::files::canonicalize(workdir).map_err(|e| backend(&e.to_string()))?;
    if !root.starts_with(&workdir) && !workdir.starts_with(&root) {
        return Err(GitStatusError::OutsideScope);
    }
    let started = Instant::now();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut hasher);
    let repository_id = GitRepositoryId(hasher.finish());
    let mut entries = BTreeMap::<PathBuf, GitStatusEntry>::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    let mut add = |repo_path: PathBuf,
                   original_path: Option<PathBuf>,
                   mut status: GitFileStatus,
                   source: GitChangeSource| {
        let absolute = workdir.join(&repo_path);
        let path = match absolute.strip_prefix(&root) {
            Ok(path) => path.to_path_buf(),
            Err(_) => {
                // A rename out of a nested workspace is still a removal inside it.
                let Some(path) = original_path.as_ref().and_then(|old| {
                    workdir
                        .join(old)
                        .strip_prefix(&root)
                        .ok()
                        .map(Path::to_path_buf)
                }) else {
                    return true;
                };
                status = GitFileStatus::Deleted;
                path
            }
        };
        let existing = entries.get(&path);
        let original_cost = original_path
            .as_ref()
            .map_or(0, |path| path.as_os_str().len());
        let old_original_cost = existing
            .and_then(|entry| entry.original_path.as_ref())
            .map_or(0, |path| path.as_os_str().len());
        let cost = if existing.is_none() {
            path.as_os_str().len()
        } else {
            0
        } + original_cost.saturating_sub(old_original_cost);
        if (existing.is_none() && entries.len() >= p.maximum_files)
            || bytes.saturating_add(cost) > p.maximum_path_bytes
        {
            truncated = true;
            return false;
        }
        bytes += cost;
        let entry = entries
            .entry(path.clone())
            .or_insert_with(|| GitStatusEntry {
                relative_path: path.clone(),
                original_path: None,
                status: status.clone(),
                staged_status: None,
                unstaged_status: None,
                sources: GitChangeSources {
                    unsaved_document_revision: unsaved_document_revision(unsaved, &path),
                    ..GitChangeSources::default()
                },
            });
        if original_path.is_some() {
            entry.original_path = original_path;
        }
        if git_status_precedence(&status) > git_status_precedence(&entry.status) {
            entry.status = status.clone();
        }
        match source {
            GitChangeSource::Staged => {
                entry.sources.staged = true;
                if entry.staged_status != Some(GitFileStatus::Conflicted) {
                    entry.staged_status = Some(status);
                }
            }
            GitChangeSource::Unstaged => {
                entry.sources.unstaged = true;
                if entry.unstaged_status != Some(GitFileStatus::Conflicted) {
                    entry.unstaged_status = Some(status);
                }
            }
            GitChangeSource::Unsaved { .. } => {}
        }
        if entry.status == GitFileStatus::Conflicted {
            entry.sources.staged = true;
            entry.sources.unstaged = true;
        }
        true
    };
    // Read the two comparisons independently: a staged rename's destination is not
    // necessarily the destination of a later worktree rename.
    let commands = [
        (
            vec![
                "diff",
                "--cached",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--",
            ],
            GitChangeSource::Staged,
        ),
        (
            vec![
                "diff",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--",
            ],
            GitChangeSource::Unstaged,
        ),
        (
            vec!["ls-files", "--others", "--exclude-standard", "-z", "--"],
            GitChangeSource::Unstaged,
        ),
    ];
    let mut output_truncated = false;
    for (arguments, source) in commands {
        let (output, clipped) = status_command(&workdir, &arguments, p, started, cancel)?;
        output_truncated |= clipped;
        // The final split item is incomplete when output was bounded; never use it as a path.
        let mut fields = output
            .split_inclusive(|byte| *byte == 0)
            .filter_map(|field| field.strip_suffix(&[0]));
        while let Some(field) = fields.next() {
            check_status_deadline(p, started, cancel)?;
            let (path, original, status) = if arguments[0] == "ls-files" {
                (field, None, GitFileStatus::Untracked)
            } else {
                let kind = field
                    .rsplit(|byte| *byte == b' ')
                    .next()
                    .and_then(|kind| kind.first())
                    .copied();
                let Some(first) = fields.next() else {
                    break;
                };
                let (path, original) = if matches!(kind, Some(b'R' | b'C')) {
                    let Some(destination) = fields.next() else {
                        break;
                    };
                    (destination, crate::git::conflict::git_path(first))
                } else {
                    (first, None)
                };
                let status = match kind {
                    Some(b'A') => GitFileStatus::Added,
                    Some(b'D') => GitFileStatus::Deleted,
                    Some(b'T') => GitFileStatus::TypeChanged,
                    Some(b'R' | b'C') => GitFileStatus::Renamed,
                    Some(b'U') => GitFileStatus::Conflicted,
                    _ => GitFileStatus::Modified,
                };
                (path, original, status)
            };
            let path = crate::git::conflict::git_path(path)
                .ok_or_else(|| backend("unsupported Git path encoding"))?;
            if !add(path, original, status, source) {
                break;
            }
        }
    }
    truncated |= output_truncated;
    // Merge unsaved-only paths without repeatedly inserting into a sorted vector.
    for document in unsaved {
        check_status_deadline(p, started, cancel)?;
        if entries.contains_key(&document.relative_path) {
            continue;
        }
        let cost = document.relative_path.as_os_str().len();
        if entries.len() >= p.maximum_files || bytes.saturating_add(cost) > p.maximum_path_bytes {
            truncated = true;
            break;
        }
        bytes += cost;
        entries.insert(
            document.relative_path.clone(),
            GitStatusEntry {
                relative_path: document.relative_path.clone(),
                original_path: None,
                status: GitFileStatus::Modified,
                staged_status: None,
                unstaged_status: None,
                sources: GitChangeSources {
                    unsaved_document_revision: Some(document.document_revision),
                    ..GitChangeSources::default()
                },
            },
        );
    }
    let entries = entries.into_values().collect();
    let mut snapshot = GitStatusSnapshot {
        request_id,
        root_revision: p.root_revision,
        repository: workdir,
        git_dir: repo.path().to_path_buf(),
        worktree_repository: crate::files::linked_worktree_repository(&root),
        scope_root: root,
        repository_id,
        entries,
        truncated,
        revision: 0,
    };
    snapshot.reconcile_unsaved_documents(unsaved, p.maximum_files, p.maximum_path_bytes);
    Ok(snapshot)
}

#[cfg(all(test, unix))]
mod tests {
    use crate::git::status::*;

    fn policy(root: &Path) -> GitStatusPolicy {
        GitStatusPolicy {
            root: root.into(),
            root_revision: 1,
            maximum_files: 100,
            maximum_path_bytes: 4096,
            deadline: Duration::from_secs(2),
        }
    }

    #[test]
    fn status_commands_enforce_deadlines_and_drain_cancelled_processes() {
        let dir = tempfile::tempdir().unwrap();
        let arguments = ["-c", "alias.dust-stall=!sleep 30", "dust-stall"];
        let mut policy = policy(dir.path());
        policy.deadline = Duration::from_millis(100);
        let started = Instant::now();
        assert!(matches!(
            status_command(
                dir.path(),
                &arguments,
                &policy,
                started,
                &AtomicBool::new(false)
            ),
            Err(GitStatusError::Deadline)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));

        policy.deadline = Duration::from_secs(10);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            worker_cancel.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert!(matches!(
            status_command(dir.path(), &arguments, &policy, started, &cancel),
            Err(GitStatusError::Cancelled)
        ));
        worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn status_refresh_waits_for_previous_worker_and_keeps_only_latest_request() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("new"), "content").unwrap();
        let (release, wait) = mpsc::channel();
        let mut service = GitStatusWorker::default();
        service.worker = Some(thread::spawn(move || {
            let _ = wait.recv();
        }));
        let mut latest = GitStatusRequestId(0);
        for _ in 0..10 {
            latest = service.start(policy(dir.path())).unwrap();
            assert!(
                service.receiver.is_none(),
                "a second scan must not start while the old worker is alive"
            );
        }
        assert_eq!(service.pending.as_ref().unwrap().0, latest);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(snapshot) = service.poll().unwrap() {
                assert_eq!(snapshot.request_id, latest);
                assert_eq!(snapshot.entries.len(), 1);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    }
}
