//! Native event batches and snapshots for readers that missed retained occurrences.

use crate::editor::text::{BufferRevision, TextBuffer};
use crate::editor::{BufferId, EditorEvent, EditorState, View, ViewId};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct BufferSnapshot {
    pub id: BufferId,
    pub path: Option<PathBuf>,
    pub text: TextBuffer,
    pub saved_revision: BufferRevision,
    pub dirty: bool,
    pub read_only: bool,
}

/// A snapshot replaces, rather than precedes, the missing event history.
#[derive(Clone, Debug)]
pub struct EventSnapshot {
    pub buffers: Vec<BufferSnapshot>,
    pub views: Vec<(ViewId, View)>,
    pub active_view: Option<ViewId>,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct EventBatch {
    pub sequence: u64,
    pub records: Vec<(u64, EditorEvent)>,
    pub snapshot: Option<EventSnapshot>,
}

/// Read all committed occurrences after `after`. Pass `None` for a new reader.
/// Text snapshots share the buffer's rope, not a materialized whole-file string.
#[must_use]
pub fn batch(state: &EditorState, after: Option<u64>) -> EventBatch {
    let sequence = state.events.sequence();
    if let Some(records) = after.and_then(|after| state.events.since(after)) {
        return EventBatch {
            sequence,
            records,
            snapshot: None,
        };
    }
    EventBatch {
        sequence,
        records: Vec::new(),
        snapshot: Some(EventSnapshot {
            buffers: state
                .buffers
                .iter()
                .map(|(id, buffer)| BufferSnapshot {
                    id,
                    path: buffer.path.clone(),
                    text: buffer.text.clone(),
                    saved_revision: buffer.saved_revision,
                    dirty: buffer.dirty,
                    read_only: buffer.read_only,
                })
                .collect(),
            views: state
                .views
                .iter()
                .map(|(id, view)| (id, view.clone()))
                .collect(),
            active_view: state.active_view(),
            workspace_root: state.workspace_root.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::batch;
    use crate::editor::text::TextBuffer;
    use crate::editor::text::{ByteOffset, ByteRange, TextEdit, TextEditSet};
    use crate::editor::{EditorEvent, EditorState};
    use crate::editor::{EditorMode, EditorOperation, EditorTransaction, TransactionId};

    #[test]
    fn batches_preserve_edits_undo_repeated_saves_and_intermediate_modes() {
        let mut state = EditorState::new("/workspace".into());
        let start = batch(&state, None).sequence;
        let (buffer, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        let revision = state.buffers[buffer].text.revision();
        state
            .apply_transaction(
                &EditorTransaction::new(TransactionId(1))
                    .with_operation(EditorOperation::SetMode {
                        view,
                        mode: EditorMode::Insert,
                    })
                    .with_operation(EditorOperation::ApplyEdits {
                        buffer,
                        edits: TextEditSet::new(
                            revision,
                            vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "x")],
                        ),
                    })
                    .with_operation(EditorOperation::Undo { buffer })
                    .with_operation(EditorOperation::SetMode {
                        view,
                        mode: EditorMode::Normal,
                    }),
            )
            .unwrap();
        let revision = state.buffers[buffer].text.revision();
        for _ in 0..2 {
            state.mark_buffer_saved_as(buffer, revision, b"abc".to_vec(), None);
        }
        let events = batch(&state, Some(start));
        assert!(events.snapshot.is_none());
        assert!(matches!(
            events.records[0].1,
            EditorEvent::BufferOpened { .. }
        ));
        assert_eq!(
            events
                .records
                .iter()
                .filter(|(_, event)| matches!(event, EditorEvent::BufferChanged { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .records
                .iter()
                .filter(|(_, event)| matches!(event, EditorEvent::BufferSaved { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .records
                .iter()
                .filter(|(_, event)| matches!(event, EditorEvent::ModeChanged { .. }))
                .count(),
            2
        );
        let changes = state.buffers[buffer]
            .changes_since(crate::editor::text::BufferRevision(0))
            .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].delta.spans()[0].inserted_len, 1);
        assert_eq!(changes[1].edits.edits()[0].replacement(), "");
        assert!(batch(&state, Some(events.sequence)).records.is_empty());
    }

    #[test]
    fn gap_returns_current_snapshot_then_delivery_resumes() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        let first = batch(&state, None);
        for _ in 0..300 {
            let revision = state.buffers[buffer].text.revision();
            state.mark_buffer_saved_as(buffer, revision, b"abc".to_vec(), None);
        }
        let gap = batch(&state, Some(first.sequence));
        assert!(gap.records.is_empty());
        let snapshot = gap.snapshot.unwrap();
        assert_eq!(snapshot.buffers[0].text.to_string(), "abc");
        assert!(!snapshot.buffers[0].dirty);
        assert_eq!(
            snapshot.buffers[0].saved_revision,
            snapshot.buffers[0].text.revision()
        );
        // A buffer still shown in a window cannot be removed, so show another one first.
        state.replace_active_view(None).unwrap();
        assert!(state.remove_buffer(buffer).is_some());
        let next = batch(&state, Some(gap.sequence));
        assert!(next.snapshot.is_none());
        assert!(next.records.iter().any(|(_, event)| matches!(
            event,
            EditorEvent::BufferClosed { buffer: closed } if *closed == buffer
        )));
    }

    #[test]
    fn rejected_transactions_publish_neither_edits_nor_events() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        let before = batch(&state, None).sequence;
        let revision = state.buffers[buffer].text.revision();
        assert!(
            state
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(1))
                        .with_operation(EditorOperation::ApplyEdits {
                            buffer,
                            edits: TextEditSet::new(
                                revision,
                                vec![TextEdit::new(ByteRange::empty(ByteOffset(0)), "x")]
                            )
                        })
                        .with_operation(EditorOperation::SetCursor {
                            view,
                            position: ByteOffset(999)
                        })
                )
                .is_err()
        );
        assert!(batch(&state, Some(before)).records.is_empty());
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }
}
