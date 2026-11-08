//! Committed editor occurrences, retained until readers need a fresh snapshot.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use crate::editor::text::{BufferRevision, ByteOffset, EditDelta, TextEditSet};
use crate::editor::{BufferId, EditorMode, ViewId};

#[derive(Clone, Debug)]
pub struct BufferChange {
    pub before: BufferRevision,
    pub after: BufferRevision,
    pub edits: TextEditSet,
    pub delta: EditDelta,
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    BufferOpened {
        buffer: BufferId,
    },
    BufferClosed {
        buffer: BufferId,
    },
    BufferChanged {
        buffer: BufferId,
        change: Arc<BufferChange>,
    },
    BufferReloaded {
        buffer: BufferId,
    },
    BufferSaved {
        buffer: BufferId,
        revision: BufferRevision,
        path: Option<PathBuf>,
    },
    CursorMoved {
        view: ViewId,
        position: ByteOffset,
    },
    ModeChanged {
        view: ViewId,
        mode: EditorMode,
    },
    WorkspaceChanged {
        root: PathBuf,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EditorEvents {
    sequence: u64,
    records: VecDeque<(u64, EditorEvent)>,
}

impl EditorEvents {
    pub(crate) fn push(&mut self, event: EditorEvent) {
        self.sequence += 1;
        self.records.push_back((self.sequence, event));
        if self.records.len() > 256 {
            self.records.pop_front();
        }
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn since(&self, sequence: u64) -> Option<Vec<(u64, EditorEvent)>> {
        if sequence > self.sequence
            || self
                .records
                .front()
                .is_some_and(|(first, _)| sequence < first - 1)
        {
            return None;
        }
        Some(
            self.records
                .iter()
                .filter(|(id, _)| *id > sequence)
                .cloned()
                .collect(),
        )
    }
}
