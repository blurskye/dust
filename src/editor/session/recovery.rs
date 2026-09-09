//! Recovery scheduling, original I/O obligations, retention and completion policy.
use super::documents::RecoveryIo;
use crate::{
    config::RecoveryConfig,
    editor::{BufferId, EditorState, text::BufferRevision},
    files::{RecoveryRecord, RequestId, VfsResult},
};
use std::ops::ControlFlow::{self, Break, Continue};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

enum Pending {
    Discovery,
    Cleanup,
    Write {
        buffer: BufferId,
        revision: BufferRevision,
        path: PathBuf,
    },
    Delete {
        path: PathBuf,
        buffer: Option<(BufferId, BufferRevision)>,
    },
}

pub(super) enum RecoveryUpdate {
    Quiet,
    Error(String),
    Pruned(usize),
    Discarded,
}

pub(super) struct RecoverySession {
    directory: PathBuf,
    records: Vec<(PathBuf, RecoveryRecord)>,
    revisions: HashMap<BufferId, BufferRevision>,
    pruned: HashMap<BufferId, BufferRevision>,
    scheduled: HashMap<BufferId, (BufferRevision, Instant)>,
    discovery_queued: bool,
    cleanup_queued: bool,
    pending: HashMap<RequestId, Pending>,
}
impl RecoverySession {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            records: Vec::new(),
            revisions: HashMap::new(),
            pruned: HashMap::new(),
            scheduled: HashMap::new(),
            discovery_queued: false,
            cleanup_queued: false,
            pending: HashMap::new(),
        }
    }
    pub(super) fn records(&self) -> &[(PathBuf, RecoveryRecord)] {
        &self.records
    }
    pub(super) fn configure(&mut self, old: &RecoveryConfig, new: &RecoveryConfig) {
        if old != new {
            self.discovery_queued = false;
            self.cleanup_queued = false;
            self.pruned.clear();
        }
        if !new.enabled {
            self.scheduled.clear();
        }
    }
    pub(super) fn path(&self, buffer: BufferId) -> PathBuf {
        let identifier = format!("{buffer:?}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        self.directory.join(format!("buffer-{identifier}.toml"))
    }
    fn recorded(&self, buffer: BufferId, revision: BufferRevision) -> bool {
        self.revisions.get(&buffer) == Some(&revision)
            || self.pruned.get(&buffer) == Some(&revision)
    }
    pub(super) fn schedule(
        &mut self,
        editor: &EditorState,
        config: &RecoveryConfig,
        now: Instant,
        exiting: bool,
        mut io: RecoveryIo<'_>,
    ) {
        if !config.enabled {
            return;
        }
        if !self.discovery_queued
            && let Ok(id) = io.discover(&self.directory, config.maximum_discovered_records)
        {
            self.discovery_queued = true;
            self.pending.insert(id, Pending::Discovery);
        }
        if config.cleanup_enabled
            && !self.cleanup_queued
            && let Ok(id) = io.cleanup(
                &self.directory,
                config.maximum_retained_records,
                config.maximum_age_days,
            )
        {
            self.cleanup_queued = true;
            self.pending.insert(id, Pending::Cleanup);
        }
        let debounce = Duration::from_millis(config.write_debounce_ms);
        for (buffer, state) in &editor.buffers {
            if state.is_virtual_read_only() {
                continue;
            }
            let revision = state.text.revision();
            if state.dirty {
                if !self.recorded(buffer, revision)
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
                self.pruned.remove(&buffer);
                if config.delete_clean_journals
                    && let Some(revision) = self.revisions.get(&buffer).copied()
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
                self.pruned.remove(&buffer);
                self.revisions.insert(buffer, revision);
                self.pending.insert(
                    id,
                    Pending::Write {
                        buffer,
                        revision,
                        path,
                    },
                );
            }
        }
    }
    pub(super) fn discard_latest(&mut self, mut io: RecoveryIo<'_>) -> Result<bool, String> {
        let Some((path, _)) = self.records.last() else {
            return Ok(false);
        };
        if self
            .pending
            .values()
            .any(|p| matches!(p, Pending::Delete { path: p, .. } if p == path))
        {
            return Ok(true);
        }
        let id = io.delete(path).map_err(|e| e.to_string())?;
        self.pending.insert(
            id,
            Pending::Delete {
                path: path.clone(),
                buffer: None,
            },
        );
        Ok(true)
    }
    pub(super) fn stopped(&mut self) {
        self.pending.clear();
        self.revisions.clear();
        self.scheduled.clear();
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
            | (Pending::Delete { path, .. }, VfsResult::Deleted { path: actual, .. }) => {
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
                let pruned =
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
                for (buffer, revision) in pruned {
                    self.revisions.remove(&buffer);
                    self.pruned.insert(buffer, revision);
                }
                if !cleanup.diagnostics.is_empty() {
                    RecoveryUpdate::Error(cleanup.diagnostics.join("; "))
                } else {
                    RecoveryUpdate::Pruned(cleanup.deleted.len())
                }
            }
            (
                Pending::Write {
                    buffer, revision, ..
                },
                VfsResult::RecoveryWritten { result, .. },
            ) => match result {
                Ok(()) => {
                    self.cleanup_queued = false;
                    RecoveryUpdate::Quiet
                }
                Err(error) => {
                    if self.revisions.get(&buffer) == Some(&revision) {
                        self.revisions.remove(&buffer);
                    }
                    RecoveryUpdate::Error(error.to_string())
                }
            },
            (Pending::Delete { path, buffer }, VfsResult::Deleted { result, .. }) => match result {
                Err(error) => RecoveryUpdate::Error(error.to_string()),
                Ok(()) => {
                    self.records.retain(|(p, _)| *p != path);
                    if let Some((buffer, revision)) = buffer {
                        if self.revisions.get(&buffer) == Some(&revision) {
                            self.revisions.remove(&buffer);
                        }
                        if self.pruned.get(&buffer) == Some(&revision) {
                            self.pruned.remove(&buffer);
                        }
                        RecoveryUpdate::Quiet
                    } else {
                        RecoveryUpdate::Discarded
                    }
                }
            },
            _ => unreachable!("original recovery result kind checked"),
        })
    }
}
#[cfg(test)]
impl RecoverySession {
    pub(super) fn skip_maintenance(&mut self) {
        self.discovery_queued = true;
        self.cleanup_queued = true;
    }
    pub(super) fn directory(&self) -> &std::path::Path {
        &self.directory
    }
    pub(super) fn scheduled(&self) -> &HashMap<BufferId, (BufferRevision, Instant)> {
        &self.scheduled
    }
    pub(super) fn revisions(&self) -> &HashMap<BufferId, BufferRevision> {
        &self.revisions
    }
    pub(super) fn pruned(&self) -> &HashMap<BufferId, BufferRevision> {
        &self.pruned
    }
    pub(super) fn writing(&self, revision: Option<BufferRevision>) -> bool {
        self.pending.values().any(|p| matches!(p, Pending::Write { revision: r, .. } if revision.is_none_or(|revision| *r == revision)))
    }
    pub(super) fn check_foreign_completions(&mut self) {
        let (id, path) = self
            .pending
            .iter()
            .find_map(|(id, pending)| match pending {
                Pending::Write { path, .. } => Some((*id, path.clone())),
                _ => None,
            })
            .expect("actual admitted recovery write");
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
    pub(super) fn deleting(&self) -> bool {
        self.pending
            .values()
            .any(|p| matches!(p, Pending::Delete { .. }))
    }
}
