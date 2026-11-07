//! Recovery scheduling, retention and completion policy.
use crate::app::documents::RecoveryIo;
use crate::config::RecoveryConfig;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::text::BufferRevision;
use crate::files::RecoveryRecord;
use crate::files::RequestId;
use crate::files::VfsResult;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::ops::ControlFlow::Break;
use std::ops::ControlFlow::Continue;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

enum Pending {
    Discovery,
    Cleanup,
    Write {
        buffer: BufferId,
        revision: BufferRevision,
        path: PathBuf,
        original_path: Option<PathBuf>,
    },
    Delete {
        path: PathBuf,
        buffer: Option<(BufferId, BufferRevision)>,
        /// Say so once the journal is gone.
        announce: bool,
    },
    CrashDelete {
        path: PathBuf,
    },
}

pub(super) enum RecoveryUpdate {
    Quiet,
    Error(String),
    Pruned(usize),
    Discarded,
    CrashCleared,
}

/// Journals Dust lists at startup, keeps on disk, and how old one may get before cleanup.
const MAXIMUM_DISCOVERED_JOURNALS: usize = 100;
const MAXIMUM_RETAINED_JOURNALS: usize = 100;
const MAXIMUM_JOURNAL_AGE_DAYS: u64 = 30;
/// How long an edit waits before its journal is written.
pub(in crate::app) const WRITE_DEBOUNCE_MS: u64 = 250;

pub(in crate::app) struct Recovery {
    directory: PathBuf,
    session: String,
    records: Vec<(PathBuf, RecoveryRecord)>,
    recovered: HashMap<BufferId, PathBuf>,
    revisions: HashMap<BufferId, BufferRevision>,
    paths: HashMap<BufferId, Option<PathBuf>>,
    closed_journals: std::collections::HashSet<PathBuf>,
    oversized: std::collections::HashSet<BufferId>,
    pub(super) oversized_notice: bool,
    scheduled: HashMap<BufferId, (BufferRevision, Instant)>,
    discovery_queued: bool,
    cleanup_queued: bool,
    pending: HashMap<RequestId, Pending>,
    /// Original files being read so a journal can be recovered into them: the journal's path
    /// and the text it holds, by read.
    reads: HashMap<RequestId, (PathBuf, String, crate::app::workspace::WorkspaceVersion)>,
}
impl Recovery {
    pub(in crate::app) fn new(directory: PathBuf) -> Self {
        static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let session = format!(
            "{}-{}-{}",
            std::process::id(),
            crate::clock::system_time()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        Self {
            directory,
            session,
            records: Vec::new(),
            recovered: HashMap::new(),
            revisions: HashMap::new(),
            paths: HashMap::new(),
            closed_journals: std::collections::HashSet::new(),
            oversized: std::collections::HashSet::new(),
            oversized_notice: false,
            scheduled: HashMap::new(),
            discovery_queued: false,
            cleanup_queued: false,
            pending: HashMap::new(),
            reads: HashMap::new(),
        }
    }
    pub(in crate::app) fn register_read(
        &mut self,
        id: RequestId,
        journal: PathBuf,
        content: String,
        workspace: crate::app::workspace::WorkspaceVersion,
    ) {
        self.reads.insert(id, (journal, content, workspace));
    }
    pub(in crate::app) fn take_read(
        &mut self,
        id: RequestId,
        workspace: crate::app::workspace::WorkspaceVersion,
    ) -> Option<(PathBuf, String)> {
        self.reads
            .remove(&id)
            .filter(|(_, _, source)| *source == workspace)
            .map(|(journal, content, _)| (journal, content))
    }
    pub(in crate::app) fn records(&self) -> &[(PathBuf, RecoveryRecord)] {
        &self.records
    }
    pub(in crate::app) fn configure(&mut self, old: &RecoveryConfig, new: &RecoveryConfig) {
        if old != new {
            self.discovery_queued = false;
            self.cleanup_queued = false;
        }
        if !new.enabled {
            self.scheduled.clear();
        }
    }
    pub(in crate::app) fn retain_recovered(&mut self, buffer: BufferId, journal: PathBuf) {
        self.records.retain(|(path, _)| path != &journal);
        self.recovered.insert(buffer, journal);
    }
    pub(in crate::app) fn path(&self, buffer: BufferId) -> PathBuf {
        if let Some(path) = self.recovered.get(&buffer) {
            return path.clone();
        }
        let identifier = format!("{buffer:?}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        self.directory
            .join(format!("buffer-{}-{identifier}.toml", self.session))
    }
    fn recorded(&self, buffer: BufferId, revision: BufferRevision, path: &Option<PathBuf>) -> bool {
        self.paths.get(&buffer) == Some(path) && self.revisions.get(&buffer) == Some(&revision)
    }
    pub(in crate::app) fn forget_buffer(&mut self, buffer: BufferId, clean: bool) {
        if clean && (self.revisions.contains_key(&buffer) || self.recovered.contains_key(&buffer)) {
            self.closed_journals.insert(self.path(buffer));
        }
        self.recovered.remove(&buffer);
        self.revisions.remove(&buffer);
        self.paths.remove(&buffer);
        self.oversized.remove(&buffer);
        self.scheduled.remove(&buffer);
    }
    pub(in crate::app) fn schedule(
        &mut self,
        editor: &EditorState,
        config: &RecoveryConfig,
        now: Instant,
        exiting: bool,
        mut io: RecoveryIo<'_>,
    ) {
        for path in &self.closed_journals {
            if !self.pending.values().any(|pending| matches!(pending, Pending::Delete { path: deleting, .. } if deleting == path))
                && let Ok(id) = io.delete(path)
            {
                self.pending.insert(id, Pending::Delete {
                    path: path.clone(), buffer: None, announce: false,
                });
            }
        }
        let closed = self
            .paths
            .keys()
            .chain(self.recovered.keys())
            .chain(self.scheduled.keys())
            .chain(self.oversized.iter())
            .filter(|buffer| !editor.buffers.contains_key(**buffer))
            .copied()
            .collect::<Vec<_>>();
        for buffer in closed {
            self.forget_buffer(buffer, false);
        }
        if !config.enabled {
            return;
        }
        if !exiting
            && !self.discovery_queued
            && let Ok(id) = io.discover(&self.directory, MAXIMUM_DISCOVERED_JOURNALS)
        {
            self.discovery_queued = true;
            self.pending.insert(id, Pending::Discovery);
        }
        if !exiting
            && !self.cleanup_queued
            && let Ok(id) = io.cleanup(
                &self.directory,
                MAXIMUM_RETAINED_JOURNALS,
                MAXIMUM_JOURNAL_AGE_DAYS,
                editor
                    .buffers
                    .iter()
                    .map(|(buffer, _)| self.path(buffer))
                    .chain(self.pending.values().filter_map(|pending| match pending {
                        Pending::Write { path, .. } => Some(path.clone()),
                        _ => None,
                    }))
                    .collect(),
            )
        {
            self.cleanup_queued = true;
            self.pending.insert(id, Pending::Cleanup);
        }
        let debounce = Duration::from_millis(WRITE_DEBOUNCE_MS);
        for (buffer, state) in &editor.buffers {
            if state.is_virtual_read_only() {
                continue;
            }
            let revision = state.text.revision();
            // Journals have a size limit: say so once rather than fail on every edit.
            if state.dirty && !crate::files::recovery_size_allowed(state.text.len_bytes() as u64) {
                self.oversized_notice |= self.oversized.insert(buffer);
                continue;
            }
            if state.dirty {
                if !self.recorded(buffer, revision, &state.path)
                    && (exiting
                        || self
                            .scheduled
                            .get(&buffer)
                            .is_none_or(|(r, _)| *r != revision))
                {
                    self.scheduled.insert(
                        buffer,
                        (revision, if exiting { now } else { now + debounce }),
                    );
                }
            } else {
                self.scheduled.remove(&buffer);
                if let Some(revision) = self
                    .revisions
                    .get(&buffer)
                    .copied()
                    .or_else(|| self.recovered.contains_key(&buffer).then_some(revision))
                {
                    let path = self.path(buffer);
                    if !self
                        .pending
                        .values()
                        .any(|p| matches!(p, Pending::Delete { path: p, .. } if *p == path))
                        && let Ok(id) = io.delete(&path)
                    {
                        self.pending.insert(
                            id,
                            Pending::Delete {
                                path,
                                buffer: Some((buffer, revision)),
                                announce: false,
                            },
                        );
                    }
                }
            }
        }
        let due = self
            .scheduled
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(id, (r, _))| (*id, *r))
            .collect::<Vec<_>>();
        for (buffer, revision) in due {
            let Some(state) = editor
                .buffers
                .get(buffer)
                .filter(|s| s.dirty && !s.is_virtual_read_only() && s.text.revision() == revision)
            else {
                self.scheduled.remove(&buffer);
                continue;
            };
            let path = self.path(buffer);
            if let Ok(id) = io.write(&path, state) {
                self.scheduled.remove(&buffer);
                self.revisions.insert(buffer, revision);
                self.paths.insert(buffer, state.path.clone());
                self.pending.insert(
                    id,
                    Pending::Write {
                        buffer,
                        revision,
                        path,
                        original_path: state.path.clone(),
                    },
                );
            }
        }
    }
    pub(in crate::app) fn clear_crash(
        &mut self,
        path: PathBuf,
        mut io: RecoveryIo<'_>,
    ) -> Result<(), String> {
        let id = io.delete(&path).map_err(|error| error.to_string())?;
        self.pending.insert(id, Pending::CrashDelete { path });
        Ok(())
    }
    /// Delete one discovered journal; the record goes once the delete completes, and
    /// `announce` says so then.
    pub(in crate::app) fn discard(
        &mut self,
        path: &Path,
        announce: bool,
        mut io: RecoveryIo<'_>,
    ) -> Result<(), String> {
        if self
            .pending
            .values()
            .any(|p| matches!(p, Pending::Delete { path: p, .. } if p == path))
        {
            return Ok(());
        }
        let id = io.delete(path).map_err(|e| e.to_string())?;
        self.pending.insert(
            id,
            Pending::Delete {
                path: path.to_owned(),
                buffer: None,
                announce,
            },
        );
        Ok(())
    }
    pub(in crate::app) fn stopped(&mut self) {
        self.pending.clear();
        self.revisions.clear();
        self.scheduled.clear();
    }
    pub(in crate::app) fn unwritten(&self, editor: &EditorState, config: &RecoveryConfig) -> usize {
        if !config.enabled {
            return 0;
        }
        editor.buffers.iter().filter(|(buffer, state)| {
            state.dirty
                && !state.is_virtual_read_only()
                && (!self.recorded(*buffer, state.text.revision(), &state.path)
                    || self.pending.values().any(|pending| {
                        matches!(pending, Pending::Write { buffer: writing, .. } if writing == buffer)
                    }))
        }).count()
    }
    pub(super) fn accept(&mut self, result: VfsResult) -> ControlFlow<RecoveryUpdate, VfsResult> {
        let id = match &result {
            VfsResult::RecoveryDiscovered { id, .. }
            | VfsResult::RecoveryCleaned { id, .. }
            | VfsResult::RecoveryWritten { id, .. }
            | VfsResult::Deleted { id, .. } => *id,
            _ => return Continue(result),
        };
        let Some(pending) = self.pending.get(&id) else {
            return Continue(result);
        };
        let matches = match (pending, &result) {
            (Pending::Discovery, VfsResult::RecoveryDiscovered { .. })
            | (Pending::Cleanup, VfsResult::RecoveryCleaned { .. }) => true,
            (Pending::Write { path, .. }, VfsResult::RecoveryWritten { path: actual, .. })
            | (Pending::Delete { path, .. }, VfsResult::Deleted { path: actual, .. })
            | (Pending::CrashDelete { path }, VfsResult::Deleted { path: actual, .. }) => {
                path == actual
            }
            _ => false,
        };
        if !matches {
            return Continue(result);
        }
        let pending = self
            .pending
            .remove(&id)
            .expect("matched original recovery request");
        Break(match (pending, result) {
            (Pending::Discovery, VfsResult::RecoveryDiscovered { discovery, .. }) => {
                self.records = discovery.records;
                if discovery.diagnostics.is_empty() {
                    RecoveryUpdate::Quiet
                } else {
                    RecoveryUpdate::Error(discovery.diagnostics.join("; "))
                }
            }
            (Pending::Cleanup, VfsResult::RecoveryCleaned { cleanup, .. }) => {
                self.records
                    .retain(|(path, _)| !cleanup.deleted.contains(path));
                let deleted =
                    self.revisions
                        .iter()
                        .filter_map(|(buffer, revision)| {
                            let writing = self.pending.values().any(
                                |p| matches!(p, Pending::Write { buffer: b, .. } if b == buffer),
                            );
                            (cleanup.deleted.contains(&self.path(*buffer)) && !writing)
                                .then_some((*buffer, *revision))
                        })
                        .collect::<Vec<_>>();
                for (buffer, _) in deleted {
                    self.revisions.remove(&buffer);
                }
                if !cleanup.diagnostics.is_empty() {
                    RecoveryUpdate::Error(cleanup.diagnostics.join("; "))
                } else {
                    RecoveryUpdate::Pruned(cleanup.deleted.len())
                }
            }
            (
                Pending::Write {
                    buffer,
                    revision,
                    original_path,
                    ..
                },
                VfsResult::RecoveryWritten { result, .. },
            ) => match result {
                Ok(()) => {
                    self.cleanup_queued = false;
                    RecoveryUpdate::Quiet
                }
                Err(error) => {
                    if self.revisions.get(&buffer) == Some(&revision)
                        && self.paths.get(&buffer) == Some(&original_path)
                    {
                        self.revisions.remove(&buffer);
                    }
                    RecoveryUpdate::Error(error.to_string())
                }
            },
            (Pending::CrashDelete { .. }, VfsResult::Deleted { result, .. }) => match result {
                Ok(()) => RecoveryUpdate::CrashCleared,
                Err(error) => RecoveryUpdate::Error(error.to_string()),
            },
            (
                Pending::Delete {
                    path,
                    buffer,
                    announce,
                },
                VfsResult::Deleted { result, .. },
            ) => match result {
                Err(error) => RecoveryUpdate::Error(error.to_string()),
                Ok(()) => {
                    self.records.retain(|(p, _)| *p != path);
                    if !self.pending.values().any(|pending| matches!(pending, Pending::Write { path: writing, .. } if *writing == path)) {
                        self.closed_journals.remove(&path);
                    }
                    if let Some((buffer, revision)) = buffer {
                        let writing = self.pending.values().any(|pending| matches!(pending, Pending::Write { buffer: writing, .. } if *writing == buffer));
                        if !writing
                            && self
                                .revisions
                                .get(&buffer)
                                .is_none_or(|current| *current == revision)
                        {
                            self.recovered.remove(&buffer);
                            self.revisions.remove(&buffer);
                            self.paths.remove(&buffer);
                        }
                        RecoveryUpdate::Quiet
                    } else if announce {
                        RecoveryUpdate::Discarded
                    } else {
                        RecoveryUpdate::Quiet
                    }
                }
            },
            _ => unreachable!("original recovery result kind checked"),
        })
    }
}
#[cfg(test)]
impl Recovery {
    pub(in crate::app) fn skip_maintenance(&mut self) {
        self.discovery_queued = true;
        self.cleanup_queued = true;
    }
    pub(in crate::app) fn directory(&self) -> &std::path::Path {
        &self.directory
    }
    pub(in crate::app) fn fixture_records(&mut self, records: Vec<(PathBuf, RecoveryRecord)>) {
        self.records = records;
    }
    pub(in crate::app) fn scheduled(&self) -> &HashMap<BufferId, (BufferRevision, Instant)> {
        &self.scheduled
    }
    pub(in crate::app) fn revisions(&self) -> &HashMap<BufferId, BufferRevision> {
        &self.revisions
    }
    pub(in crate::app) fn writing(&self, revision: Option<BufferRevision>) -> bool {
        self.pending.values().any(|p| matches!(p, Pending::Write { revision: r, .. } if revision.is_none_or(|revision| *r == revision)))
    }
    pub(in crate::app) fn check_foreign_completions(&mut self) {
        let (id, path) = self
            .pending
            .iter()
            .find_map(|(id, pending)| match pending {
                Pending::Write { path, .. } => Some((*id, path.clone())),
                _ => None,
            })
            .expect("actual accepted recovery write");
        let count = self.pending.len();
        assert!(
            self.accept(VfsResult::Deleted {
                id,
                path: path.clone(),
                result: Ok(())
            })
            .is_continue()
        );
        assert!(
            self.accept(VfsResult::RecoveryWritten {
                id,
                path: path.with_extension("foreign"),
                result: Ok(())
            })
            .is_continue()
        );
        assert_eq!(self.pending.len(), count);
        assert!(self.pending.contains_key(&id));
    }
    pub(in crate::app) fn deleting(&self) -> bool {
        self.pending
            .values()
            .any(|p| matches!(p, Pending::Delete { .. }))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::documents::recovery::*;
    use crate::editor::text::TextBuffer;

    #[test]
    fn recovery_retiring_a_buffer_drops_tracking_but_keeps_pending_writes() {
        let directory = tempfile::tempdir().unwrap();
        let mut recovery = Recovery::new(directory.path().into());
        let mut editor = EditorState::new(directory.path().into());
        let (buffer, _) = editor
            .open_text(TextBuffer::from_text("draft"), None)
            .unwrap();
        let revision = editor.buffers[buffer].text.revision();
        let path = recovery.path(buffer);
        crate::files::write_recovery(
            &path,
            &RecoveryRecord::new(None, revision.0, "draft".into()),
        )
        .unwrap();
        recovery.recovered.insert(buffer, path.clone());
        recovery.revisions.insert(buffer, revision);
        recovery.paths.insert(buffer, None);
        recovery
            .scheduled
            .insert(buffer, (revision, Instant::now()));
        recovery.oversized.insert(buffer);
        let id = RequestId(1);
        recovery.pending.insert(
            id,
            Pending::Write {
                buffer,
                revision,
                path: path.clone(),
                original_path: None,
            },
        );

        recovery.forget_buffer(buffer, false);
        assert!(recovery.recovered.is_empty());
        assert!(recovery.revisions.is_empty());
        assert!(recovery.paths.is_empty());
        assert!(recovery.scheduled.is_empty());
        assert!(recovery.oversized.is_empty());
        assert!(recovery.pending.contains_key(&id));
        assert!(
            path.exists(),
            "closing a dirty buffer preserves its journal"
        );
        assert!(
            recovery
                .accept(VfsResult::RecoveryWritten {
                    id,
                    path,
                    result: Ok(())
                })
                .is_break()
        );
        assert!(recovery.pending.is_empty());
        assert!(recovery.revisions.is_empty());
        assert!(recovery.paths.is_empty());
    }
}
