//! Text deltas and position mapping.

use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaSpan {
    pub old: ByteRange,
    pub inserted_len: usize,
}

/// Maps positions from the pre-edit revision to the resulting revision.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditDelta {
    spans: Vec<DeltaSpan>,
}

impl EditDelta {
    #[must_use]
    pub fn new(mut spans: Vec<DeltaSpan>) -> Self {
        spans.sort_by_key(|span| span.old.start);
        Self { spans }
    }

    #[must_use]
    pub fn spans(&self) -> &[DeltaSpan] {
        &self.spans
    }

    #[must_use]
    pub fn map_offset(&self, offset: ByteOffset, affinity: Affinity) -> ByteOffset {
        let mut shift: i128 = 0;
        for span in &self.spans {
            let start = span.old.start.get();
            let end = span.old.end.get();
            if offset.get() < start || (offset.get() == start && affinity == Affinity::Before) {
                break;
            }
            if offset.get() < end
                || (offset.get() == start && start == end && affinity == Affinity::Before)
            {
                let mapped = i128::try_from(start).expect("usize always fits i128")
                    + shift
                    + if affinity == Affinity::After {
                        i128::try_from(span.inserted_len).expect("usize always fits i128")
                    } else {
                        0
                    };
                return ByteOffset(
                    usize::try_from(mapped).expect("validated deltas stay nonnegative"),
                );
            }
            shift += i128::try_from(span.inserted_len).expect("usize always fits i128")
                - i128::try_from(span.old.len()).expect("usize always fits i128");
        }
        let mapped = i128::try_from(offset.get()).expect("usize always fits i128") + shift;
        ByteOffset(usize::try_from(mapped).expect("validated deltas stay nonnegative"))
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::text::Affinity;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::DeltaSpan;
    use crate::editor::text::EditDelta;

    #[test]
    fn adjacent_replacement_and_insertion_both_move_the_boundary() {
        let delta = EditDelta::new(vec![
            DeltaSpan {
                old: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                inserted_len: 2,
            },
            DeltaSpan {
                old: ByteRange::empty(ByteOffset(1)),
                inserted_len: 1,
            },
        ]);
        assert_eq!(
            delta.map_offset(ByteOffset(1), Affinity::After),
            ByteOffset(3)
        );
        assert_eq!(
            delta.map_offset(ByteOffset(1), Affinity::Before),
            ByteOffset(2)
        );
    }

    #[test]
    fn affinity_controls_insert_boundary() {
        let delta = EditDelta::new(vec![DeltaSpan {
            old: ByteRange::empty(ByteOffset(2)),
            inserted_len: 3,
        }]);
        assert_eq!(
            delta.map_offset(ByteOffset(2), Affinity::Before),
            ByteOffset(2)
        );
        assert_eq!(
            delta.map_offset(ByteOffset(2), Affinity::After),
            ByteOffset(5)
        );
        assert_eq!(
            delta.map_offset(ByteOffset(4), Affinity::After),
            ByteOffset(7)
        );
    }
}
