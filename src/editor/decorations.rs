//! Buffer-local anchors shared by signs, highlights and virtual text.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::editor::text::{Affinity, ByteRange, EditDelta, TextBuffer};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct MarkId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Gravity {
    Left,
    Right,
}

impl Gravity {
    fn affinity(self) -> Affinity {
        match self {
            Self::Left => Affinity::Before,
            Self::Right => Affinity::After,
        }
    }
}

#[derive(Clone, Debug)]
struct Mark {
    namespace: String,
    range: ByteRange,
    start_gravity: Gravity,
    end_gravity: Gravity,
    decoration: Decoration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VirtualTextPosition {
    Inline,
    EndOfLine,
    Overlay,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StyledText {
    pub(crate) text: String,
    pub(crate) style: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VirtualText {
    pub(crate) text: StyledText,
    pub(crate) position: VirtualTextPosition,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Decoration {
    pub(crate) highlight: Option<String>,
    pub(crate) virtual_text: Option<VirtualText>,
    pub(crate) sign: Option<StyledText>,
}

/// Positions are bytes in the current buffer. Deletion collapses anchors; undo
/// restores positions of surviving marks, but never resurrects removed marks.
#[derive(Clone, Debug, Default)]
pub(crate) struct Decorations {
    marks: BTreeMap<MarkId, Mark>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MarkPositions(Vec<(MarkId, ByteRange)>);

impl Decorations {
    pub(crate) fn insert(
        &mut self,
        text: &TextBuffer,
        namespace: &str,
        range: ByteRange,
        start_gravity: Gravity,
        end_gravity: Gravity,
    ) -> Option<MarkId> {
        if range.start > range.end
            || text.validate_offset(range.start).is_err()
            || text.validate_offset(range.end).is_err()
        {
            return None;
        }
        // IDs must not alias marks from a reloaded buffer or another buffer.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = MarkId(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .ok()?,
        );
        self.marks.insert(
            id,
            Mark {
                namespace: namespace.into(),
                range,
                start_gravity,
                end_gravity,
                decoration: Decoration::default(),
            },
        );
        Some(id)
    }

    pub(crate) fn range(&self, id: MarkId) -> Option<ByteRange> {
        self.marks.get(&id).map(|mark| mark.range)
    }

    pub(crate) fn decorate(&mut self, id: MarkId, decoration: Decoration) {
        if let Some(mark) = self.marks.get_mut(&id) {
            mark.decoration = decoration;
        }
    }

    pub(crate) fn drawing(&self) -> impl Iterator<Item = (ByteRange, &Decoration)> {
        self.marks
            .values()
            .map(|mark| (mark.range, &mark.decoration))
    }

    pub(crate) fn remove(&mut self, id: MarkId) {
        self.marks.remove(&id);
    }

    pub(crate) fn clear(&mut self, namespace: &str) {
        self.marks.retain(|_, mark| mark.namespace != namespace);
    }

    pub(crate) fn advance(&mut self, delta: &EditDelta) {
        for mark in self.marks.values_mut() {
            let start = delta.map_offset(mark.range.start, mark.start_gravity.affinity());
            let end = delta.map_offset(mark.range.end, mark.end_gravity.affinity());
            // Opposing gravities on an empty range must not make a reversed range.
            mark.range = ByteRange {
                start,
                end: end.max(start),
            };
        }
    }

    pub(crate) fn positions(&self) -> MarkPositions {
        MarkPositions(
            self.marks
                .iter()
                .map(|(id, mark)| (*id, mark.range))
                .collect(),
        )
    }

    pub(crate) fn restore_positions(&mut self, positions: &MarkPositions) {
        for (id, range) in &positions.0 {
            if let Some(mark) = self.marks.get_mut(id) {
                mark.range = *range;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Decorations, Gravity};
    use crate::editor::text::{ByteOffset, DeltaSpan, TextEdit, TextEditSet};
    use crate::editor::text::{ByteRange, EditDelta, TextBuffer};
    use crate::editor::{
        EditorOperation, EditorState, EditorTransaction, TransactionId, UndoPolicy,
    };

    fn apply(state: &mut EditorState, operation: EditorOperation) {
        state
            .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(operation))
            .unwrap();
    }

    #[test]
    fn deletion_undo_and_redo_restore_both_gravities_and_empty_marks() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state
            .open_text(TextBuffer::from_text("abcdef"), None)
            .unwrap();
        let buffer = &mut state.buffers[id];
        let mut marks = Vec::new();
        for gravity in [Gravity::Left, Gravity::Right] {
            for range in [
                ByteRange::empty(ByteOffset(3)),
                ByteRange::new(ByteOffset(2), ByteOffset(4)).unwrap(),
            ] {
                let mark = buffer
                    .decorations
                    .insert(&buffer.text, "test", range, gravity, gravity)
                    .unwrap();
                marks.push((mark, range));
            }
        }
        let revision = buffer.text.revision();
        apply(
            &mut state,
            EditorOperation::ApplyEdits {
                buffer: id,
                edits: TextEditSet::new(
                    revision,
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(1), ByteOffset(5)).unwrap(),
                        "",
                    )],
                ),
            },
        );
        for (mark, _) in &marks {
            assert_eq!(
                state.buffers[id].decorations.range(*mark),
                Some(ByteRange::empty(ByteOffset(1)))
            );
        }
        apply(&mut state, EditorOperation::Undo { buffer: id });
        for (mark, range) in &marks {
            assert_eq!(state.buffers[id].decorations.range(*mark), Some(*range));
        }
        apply(&mut state, EditorOperation::Redo { buffer: id });
        for (mark, _) in &marks {
            assert_eq!(
                state.buffers[id].decorations.range(*mark),
                Some(ByteRange::empty(ByteOffset(1)))
            );
        }
    }

    #[test]
    fn merged_insertions_undo_redo_and_removed_marks_stay_removed() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state.open_text(TextBuffer::from_text("ab"), None).unwrap();
        let buffer = &mut state.buffers[id];
        let left = buffer
            .decorations
            .insert(
                &buffer.text,
                "left",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Left,
                Gravity::Left,
            )
            .unwrap();
        let right = buffer
            .decorations
            .insert(
                &buffer.text,
                "right",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Right,
                Gravity::Right,
            )
            .unwrap();
        for at in [1, 2] {
            let revision = state.buffers[id].text.revision();
            state
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(1))
                        .with_undo_policy(UndoPolicy::Merge)
                        .with_operation(EditorOperation::ApplyEdits {
                            buffer: id,
                            edits: TextEditSet::new(
                                revision,
                                vec![TextEdit::new(ByteRange::empty(ByteOffset(at)), "x")],
                            ),
                        }),
                )
                .unwrap();
        }
        assert_eq!(
            state.buffers[id].decorations.range(left),
            Some(ByteRange::empty(ByteOffset(1)))
        );
        assert_eq!(
            state.buffers[id].decorations.range(right),
            Some(ByteRange::empty(ByteOffset(3)))
        );
        apply(&mut state, EditorOperation::Undo { buffer: id });
        assert_eq!(
            state.buffers[id].decorations.range(right),
            Some(ByteRange::empty(ByteOffset(1)))
        );
        state.buffers[id].decorations.clear("left");
        apply(&mut state, EditorOperation::Redo { buffer: id });
        assert_eq!(
            state.buffers[id].decorations.range(right),
            Some(ByteRange::empty(ByteOffset(3)))
        );
        assert_eq!(state.buffers[id].decorations.range(left), None);
    }

    #[test]
    fn reload_drops_marks_and_ids_never_alias_another_buffer() {
        let mut state = EditorState::new("/workspace".into());
        let (id, _) = state.open_text(TextBuffer::from_text("ab"), None).unwrap();
        let buffer = &mut state.buffers[id];
        let old = buffer
            .decorations
            .insert(
                &buffer.text,
                "test",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Right,
                Gravity::Right,
            )
            .unwrap();
        state.replace_buffer_from_disk(id, TextBuffer::from_text("new"), "/file".into());
        let buffer = &mut state.buffers[id];
        let new = buffer
            .decorations
            .insert(
                &buffer.text,
                "test",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Right,
                Gravity::Right,
            )
            .unwrap();
        assert_ne!(old, new);
        assert!(buffer.decorations.range(old).is_none());
    }

    #[test]
    fn insertion_gravity_includes_zero_width_marks() {
        let text = TextBuffer::from_text("abc");
        let mut marks = Decorations::default();
        let left = marks
            .insert(
                &text,
                "test",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Left,
                Gravity::Left,
            )
            .unwrap();
        let right = marks
            .insert(
                &text,
                "test",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Right,
                Gravity::Right,
            )
            .unwrap();
        let expanded = marks
            .insert(
                &text,
                "test",
                ByteRange::empty(ByteOffset(1)),
                Gravity::Left,
                Gravity::Right,
            )
            .unwrap();
        marks.advance(&EditDelta::new(vec![DeltaSpan {
            old: ByteRange::empty(ByteOffset(1)),
            inserted_len: 2,
        }]));
        assert_eq!(marks.range(left), Some(ByteRange::empty(ByteOffset(1))));
        assert_eq!(marks.range(right), Some(ByteRange::empty(ByteOffset(3))));
        assert_eq!(
            marks.range(expanded),
            Some(ByteRange::new(ByteOffset(1), ByteOffset(3)).unwrap())
        );
    }

    #[test]
    fn namespaces_and_character_boundaries_are_checked() {
        let text = TextBuffer::from_text("é");
        let mut marks = Decorations::default();
        assert!(
            marks
                .insert(
                    &text,
                    "a",
                    ByteRange::empty(ByteOffset(1)),
                    Gravity::Left,
                    Gravity::Left
                )
                .is_none()
        );
        let a = marks
            .insert(
                &text,
                "a",
                ByteRange::empty(ByteOffset(0)),
                Gravity::Left,
                Gravity::Left,
            )
            .unwrap();
        let b = marks
            .insert(
                &text,
                "b",
                ByteRange::empty(ByteOffset(2)),
                Gravity::Right,
                Gravity::Right,
            )
            .unwrap();
        let saved = marks.positions();
        marks.clear("a");
        marks.restore_positions(&saved);
        assert_eq!(marks.range(a), None);
        assert_eq!(marks.range(b), Some(ByteRange::empty(ByteOffset(2))));
    }
}
