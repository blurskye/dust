//! Task-neutral worker routes. Parser state and document/run authority stay in TaskSession.
use super::{DocumentSession, RequestId, VfsResult};
use std::sync::Arc;
#[derive(Clone)]
pub(in crate::editor::session) struct TaskReadKey {
    worker: Arc<()>,
    id: RequestId,
}
impl TaskReadKey {
    pub(in crate::editor::session) fn id(&self) -> RequestId {
        self.id
    }
    pub(in crate::editor::session) fn same(&self, other: &Self) -> bool {
        self.id == other.id && self.from_worker(&other.worker)
    }
    pub(in crate::editor::session) fn from_worker(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
}
pub(in crate::editor::session) struct TaskSubmission {
    result: Result<Option<TaskReadKey>, String>,
}
impl TaskSubmission {
    pub(in crate::editor::session) fn into_result(self) -> Result<Option<TaskReadKey>, String> {
        self.result
    }
}
pub(in crate::editor::session) struct TaskCancellation {
    key: TaskReadKey,
}
impl TaskCancellation {
    pub(in crate::editor::session) fn into_key(self) -> TaskReadKey {
        self.key
    }
}
pub(in crate::editor::session) struct TaskReadReply {
    key: TaskReadKey,
    result: VfsResult,
}
impl TaskReadReply {
    pub(in crate::editor::session) fn id(&self) -> RequestId {
        self.key.id
    }
    pub(in crate::editor::session) fn matches(&self, key: &TaskReadKey) -> bool {
        self.key.same(key)
    }
    pub(in crate::editor::session) fn into_result(self) -> VfsResult {
        self.result
    }
    pub(super) fn new(worker: Arc<()>, result: VfsResult) -> Self {
        Self {
            key: TaskReadKey {
                worker,
                id: result.request_id(),
            },
            result,
        }
    }
}
pub(in crate::editor::session) struct TaskReadStopped {
    worker: Arc<()>,
}
impl TaskReadStopped {
    pub(super) fn new(worker: Arc<()>) -> Self {
        Self { worker }
    }
    pub(in crate::editor::session) fn matches(&self, key: &TaskReadKey) -> bool {
        key.from_worker(&self.worker)
    }
}
impl DocumentSession {
    pub(in crate::editor::session) fn submit_linter_read(
        &mut self,
        loan: crate::editor::session::tasks::LinterReadLoan<'_>,
    ) -> Result<bool, String> {
        let result = (|| {
            match self.check_admission() {
                Ok(()) => {}
                Err(crate::files::VfsError::QueueFull) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            }
            // A borrowed real queue slot/ID precedes cloning path/root request metadata.
            let admission = match self.vfs.reserve_read() {
                Ok(admission) => admission,
                Err(crate::files::VfsError::QueueFull) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let (path, roots, maximum) = loan.request(&self.owner)?;
            let id = match admission.submit(path, roots, maximum) {
                Ok(id) => id,
                Err(crate::files::VfsError::QueueFull) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            self.pending_tasks.insert(id);
            Ok(Some(TaskReadKey {
                worker: self.owner.clone(),
                id,
            }))
        })();
        loan.finish(TaskSubmission { result })
    }
    pub(in crate::editor::session) fn cancel_linter_read(
        &mut self,
        key: &TaskReadKey,
    ) -> Option<TaskCancellation> {
        if key.from_worker(&self.owner)
            && self.pending_tasks.contains(&key.id)
            && self.vfs.cancel(key.id)
        {
            self.pending_tasks.remove(&key.id);
            Some(TaskCancellation { key: key.clone() })
        } else {
            None
        }
    }
}
