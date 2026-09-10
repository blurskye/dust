//! Persistence I/O, history, capture and original restore obligations.
//! Dispatch composes other owners' snapshots and consumes checked editor/window plans.
use crate::persistence::{
    PersistedState, PersistenceError, PersistenceEvent, PersistenceId, PersistencePolicy,
    PersistenceService,
};
use std::path::PathBuf;
#[path = "persistence/session.rs"]
mod session;
#[cfg(test)]
pub(in crate::editor::session) use session::capture_persisted_layout;
pub(in crate::editor::session) use session::{RestoreContext, RestoreReady, RestoreUpdate};

pub(in crate::editor::session) struct PersistenceSession {
    load_source: Option<session::RestoreSource>,
    restore: Option<session::PendingRestore>,
    retired_reads: std::collections::HashSet<crate::files::RequestId>,
    recent_files: Vec<PathBuf>,
    marks: Vec<crate::persistence::PersistedMark>,
    jumps: Vec<crate::persistence::PersistedLocation>,
    changes: Vec<crate::persistence::PersistedLocation>,
    worker: PersistenceService,
    path: PathBuf,
    pending: Option<(PersistenceId, bool)>,
    save_requested: bool,
    failure: Option<String>,
    closing: bool,
    save_expected: bool,
    saved_on_exit: bool,
    exit_request: Option<PersistenceId>,
    navigation_reads:
        std::collections::HashMap<crate::files::RequestId, crate::persistence::PersistedLocation>,
}
impl PersistenceSession {
    pub(in crate::editor::session) fn new(worker: PersistenceService, path: PathBuf) -> Self {
        Self {
            load_source: None,
            restore: None,
            retired_reads: Default::default(),
            recent_files: Vec::new(),
            marks: Vec::new(),
            jumps: Vec::new(),
            changes: Vec::new(),
            worker,
            path,
            pending: None,
            save_requested: false,
            failure: None,
            closing: false,
            save_expected: false,
            saved_on_exit: false,
            exit_request: None,
            navigation_reads: Default::default(),
        }
    }
    pub(in crate::editor::session) fn register_navigation_read(
        &mut self,
        id: crate::files::RequestId,
        location: crate::persistence::PersistedLocation,
    ) {
        self.navigation_reads.insert(id, location);
    }
    pub(in crate::editor::session) fn take_navigation_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<crate::persistence::PersistedLocation> {
        self.navigation_reads.remove(&id)
    }
    pub(in crate::editor::session) fn retire_document_request(
        &mut self,
        id: crate::files::RequestId,
    ) {
        self.navigation_reads.remove(&id);
    }
    pub(in crate::editor::session) fn request_save(&mut self) {
        if !self.closing {
            self.save_requested = true;
        }
    }
    pub(in crate::editor::session) fn take_save_request(&mut self) -> bool {
        !self.pending() && !self.closing && std::mem::take(&mut self.save_requested)
    }
    pub(in crate::editor::session) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(in crate::editor::session) fn load(
        &mut self,
        policy: PersistencePolicy,
    ) -> Result<(), String> {
        if self.pending() || self.closing {
            return Err("persistence is busy or closing".into());
        }
        let id = self
            .worker
            .read(self.path.clone(), policy)
            .map_err(|e| self.failed(e))?;
        self.pending = Some((id, false));
        self.failure = None;
        Ok(())
    }
    pub(in crate::editor::session) fn save(
        &mut self,
        state: PersistedState,
        policy: PersistencePolicy,
    ) -> Result<(), String> {
        if self.pending() {
            return Err("persistence is already pending".into());
        }
        self.save_requested = false;
        let id = self
            .worker
            .write(self.path.clone(), state, policy)
            .map_err(|e| self.failed(e))?;
        self.pending = Some((id, true));
        if self.closing {
            self.exit_request = Some(id);
            self.saved_on_exit = false;
        }
        self.failure = None;
        Ok(())
    }
    fn failed(&mut self, error: impl std::fmt::Display) -> String {
        let message = error.to_string().chars().take(1024).collect::<String>();
        self.failure = Some(message.clone());
        message
    }
    pub(in crate::editor::session) fn poll(
        &mut self,
    ) -> Option<Result<Box<PersistedState>, String>> {
        loop {
            let event = match self.worker.try_event() {
                Ok(Some(event)) => event,
                Ok(None) => return None,
                Err(error) => {
                    self.pending.take()?;
                    return Some(Err(self.failed(error)));
                }
            };
            let generation = match &event {
                PersistenceEvent::Loaded { generation, .. }
                | PersistenceEvent::Saved { generation }
                | PersistenceEvent::Failed { generation, .. } => *generation,
            };
            let Some((expected, saving)) = self.pending else {
                continue;
            };
            if expected != generation {
                continue;
            }
            self.pending = None;
            match event {
                PersistenceEvent::Loaded { state, .. } if !saving && !self.closing => {
                    return Some(Ok(state));
                }
                PersistenceEvent::Loaded { .. } if !saving => {}
                PersistenceEvent::Saved { .. } if saving => {
                    self.saved_on_exit = self.exit_request == Some(generation);
                }
                PersistenceEvent::Failed {
                    error: PersistenceError::Io(error),
                    ..
                } if !saving && error.kind() == std::io::ErrorKind::NotFound => {}
                PersistenceEvent::Failed { error, .. } => return Some(Err(self.failed(error))),
                _ => return Some(Err(self.failed("unexpected persistence terminal result"))),
            }
        }
    }
    pub(in crate::editor::session) fn begin_shutdown(&mut self, save_expected: bool) {
        self.closing = true;
        self.save_expected = save_expected;
        self.saved_on_exit = false;
        self.exit_request = None;
    }
    pub(in crate::editor::session) fn shutdown_result(&self) -> Result<(), String> {
        if self.pending() {
            return Err("persistence did not finish before the exit deadline; an admitted write may still complete".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.save_expected && !self.saved_on_exit {
            return Err(
                "exit snapshot was not saved; previous persistent state was not replaced".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn drain(owner: &mut PersistenceSession) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while owner.pending() {
            owner.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn state_save_requests_coalesce_without_consuming_a_pending_write() {
        let root = tempfile::tempdir().unwrap();
        let (worker, release) = PersistenceService::gated_for_test(1);
        let mut owner = PersistenceSession::new(worker, root.path().join("state.json"));
        owner
            .save(PersistedState::default(), PersistencePolicy::default())
            .unwrap();
        owner.request_save();
        owner.request_save();
        assert!(!owner.take_save_request());
        drop(release);
        drain(&mut owner);
        assert!(owner.take_save_request());
        assert!(!owner.take_save_request());
        owner.begin_shutdown(true);
        owner.request_save();
        assert!(!owner.take_save_request());
        assert!(
            owner.shutdown_result().is_err(),
            "an earlier write is not the exit snapshot"
        );
    }
    #[test]
    fn full_and_stopped_exit_admission_are_not_successful_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let (mut worker, release) = PersistenceService::gated_for_test(1);
        worker
            .read(path.clone(), PersistencePolicy::default())
            .unwrap();
        let mut full = PersistenceSession::new(worker, path.clone());
        full.begin_shutdown(true);
        assert!(
            full.save(PersistedState::default(), PersistencePolicy::default())
                .is_err()
        );
        assert!(full.shutdown_result().is_err());
        drop(full);
        drop(release);
        let mut stopped = PersistenceSession::new(PersistenceService::stopped_for_test(), path);
        stopped.begin_shutdown(true);
        assert!(
            stopped
                .save(PersistedState::default(), PersistencePolicy::default())
                .is_err()
        );
        assert!(stopped.shutdown_result().is_err());
    }
    #[test]
    fn earlier_save_finishing_during_exit_does_not_acknowledge_the_exit_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (worker, release) = PersistenceService::gated_for_test(1);
        let mut owner = PersistenceSession::new(worker, directory.path().join("state.json"));
        owner
            .save(PersistedState::default(), PersistencePolicy::default())
            .unwrap();
        owner.begin_shutdown(true);
        release.send(()).unwrap();
        drain(&mut owner);
        assert!(
            owner.shutdown_result().is_err(),
            "earlier save incorrectly acknowledged an exit snapshot that was never admitted"
        );
        owner
            .save(PersistedState::default(), PersistencePolicy::default())
            .unwrap();
        drain(&mut owner);
        owner.shutdown_result().unwrap();
    }
    #[test]
    fn exit_requires_a_completed_snapshot_not_idle_or_failed_admission() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut owner = PersistenceSession::new(PersistenceService::start(1), path.clone());
        owner.load(PersistencePolicy::default()).unwrap();
        owner.begin_shutdown(true);
        drain(&mut owner);
        assert!(
            owner.shutdown_result().is_err(),
            "a completed load is not an exit save"
        );
        owner
            .save(PersistedState::default(), PersistencePolicy::default())
            .unwrap();
        assert!(owner.shutdown_result().is_err());
        drain(&mut owner);
        owner.shutdown_result().unwrap();
        assert!(path.is_file());
        let mut rejected =
            PersistenceSession::new(PersistenceService::start(1), directory.path().to_path_buf());
        rejected.begin_shutdown(true);
        rejected
            .save(PersistedState::default(), PersistencePolicy::default())
            .unwrap();
        drain(&mut rejected);
        assert!(
            rejected.shutdown_result().is_err(),
            "failed write must not look idle/successful"
        );
    }
}
