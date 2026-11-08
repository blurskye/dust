//! Task-neutral worker routes. Parser state and document/run authority stay in Tasks.
use crate::app::documents::Documents;
use crate::files::RequestId;
use crate::files::VfsResult;
use std::sync::Arc;
#[derive(Clone)]
pub(in crate::app) struct TaskReadKey {
    worker: Arc<()>,
    id: RequestId,
}
impl TaskReadKey {
    pub(in crate::app) fn id(&self) -> RequestId {
        self.id
    }
    pub(in crate::app) fn same(&self, other: &Self) -> bool {
        self.id == other.id && self.belongs_to_worker(&other.worker)
    }
    pub(in crate::app) fn belongs_to_worker(&self, worker: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
}
pub(in crate::app) struct TaskSubmission {
    result: Result<Option<TaskReadKey>, String>,
}
impl TaskSubmission {
    pub(in crate::app) fn into_result(self) -> Result<Option<TaskReadKey>, String> {
        self.result
    }
}
pub(in crate::app) struct TaskCancellation {
    key: TaskReadKey,
}
impl TaskCancellation {
    pub(in crate::app) fn into_key(self) -> TaskReadKey {
        self.key
    }
}
pub(in crate::app) struct TaskReadReply {
    key: TaskReadKey,
    result: VfsResult,
}
impl TaskReadReply {
    pub(in crate::app) fn id(&self) -> RequestId {
        self.key.id
    }
    pub(in crate::app) fn matches(&self, key: &TaskReadKey) -> bool {
        self.key.same(key)
    }
    pub(in crate::app) fn into_result(self) -> VfsResult {
        self.result
    }
    pub(in crate::app) fn new(worker: Arc<()>, result: VfsResult) -> Self {
        Self {
            key: TaskReadKey {
                worker,
                id: result.request_id(),
            },
            result,
        }
    }
}
pub(in crate::app) struct TaskReadStopped {
    worker: Arc<()>,
}
impl TaskReadStopped {
    pub(in crate::app) fn new(worker: Arc<()>) -> Self {
        Self { worker }
    }
    pub(in crate::app) fn matches(&self, key: &TaskReadKey) -> bool {
        key.belongs_to_worker(&self.worker)
    }
}
impl Documents {
    pub(in crate::app) fn submit_linter_read(
        &mut self,
        loan: crate::app::tasks::LinterReadLoan<'_>,
    ) -> Result<bool, String> {
        let result = (|| {
            match self.check_capacity() {
                Ok(()) => {}
                Err(crate::files::VfsError::QueueFull) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            }
            // A borrowed real queue slot/ID precedes cloning path/root request metadata.
            let reservation = match self.vfs.reserve_read() {
                Ok(reservation) => reservation,
                Err(crate::files::VfsError::QueueFull) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let (path, roots, maximum) = loan.request(&self.owner)?;
            let id = match reservation.submit(path, roots, maximum) {
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
    pub(in crate::app) fn cancel_linter_read(
        &mut self,
        key: &TaskReadKey,
    ) -> Option<TaskCancellation> {
        if key.belongs_to_worker(&self.owner)
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
