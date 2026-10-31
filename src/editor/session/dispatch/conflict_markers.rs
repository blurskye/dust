//! Accepting one conflict block at a time from its markers, like VS Code's Accept Current,
//! Accept Incoming and Accept Both. The whole-file choices live in `workspace::git::conflict`.

use super::{
    ByteOffset, ByteRange, EditorOperation, EditorTransaction, InteractiveApplication,
    Precondition, TextEdit, TextEditSet, UndoPolicy,
};

/// Line numbers of one block's markers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConflictBlock {
    start: usize,
    /// `|||||||` from diff3 style, which ends the ours side early.
    base: Option<usize>,
    separator: usize,
    end: usize,
}

/// The block around `line`, or else the next one after it.
fn conflict_block_at(lines: &[&str], line: usize) -> Option<ConflictBlock> {
    let mut found = None;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].starts_with("<<<<<<<") {
            let block = read_block(lines, index)?;
            if line <= block.end {
                return Some(block);
            }
            found = Some(block);
            index = block.end;
        }
        index += 1;
    }
    // Past the last block the cursor still means the nearest one.
    found
}

fn read_block(lines: &[&str], start: usize) -> Option<ConflictBlock> {
    let mut base = None;
    let mut separator = None;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if line.starts_with("|||||||") && separator.is_none() {
            base = Some(index);
        } else if line.starts_with("=======") && separator.is_none() {
            separator = Some(index);
        } else if line.starts_with(">>>>>>>") {
            return Some(ConflictBlock {
                start,
                base,
                separator: separator?,
                end: index,
            });
        }
    }
    None
}

/// What a block becomes for `side`: `ours`, `theirs` or `both`.
fn resolved_text(lines: &[&str], block: ConflictBlock, side: &str) -> String {
    let ours = &lines[block.start + 1..block.base.unwrap_or(block.separator)];
    let theirs = &lines[block.separator + 1..block.end];
    let kept = match side {
        "ours" => ours.to_vec(),
        "theirs" => theirs.to_vec(),
        _ => [ours, theirs].concat(),
    };
    kept.iter().map(|line| format!("{line}\n")).collect()
}

impl InteractiveApplication {
    pub(super) fn accept_conflict_block(&mut self, side: &str) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let text = &self.state.buffers[buffer_id].text;
        let revision = text.revision();
        let cursor_line = text.byte_to_line(view.cursor).unwrap_or(0);
        let source = text.to_string();
        let lines = source.lines().collect::<Vec<_>>();
        let Some(block) = conflict_block_at(&lines, cursor_line) else {
            self.show_error("no conflict markers in this file");
            return;
        };
        let replacement = resolved_text(&lines, block, side);
        let range = text.line_to_byte(block.start).and_then(|start| {
            let end = text
                .line_to_byte(block.end + 1)
                .unwrap_or(ByteOffset(text.len_bytes()));
            Ok((start, end))
        });
        let Some(range) = range.ok().and_then(|(start, end)| ByteRange::new(start, end).ok()) else {
            self.show_error("the conflict block could not be located");
            return;
        };
        let landing = ByteOffset(range.start.get());
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(revision, vec![TextEdit::new(range, replacement)]),
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: landing,
            })
            .with_undo_policy(UndoPolicy::Record);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            return;
        }
        let left = self.state.buffers[buffer_id]
            .text
            .to_string()
            .lines()
            .filter(|line| line.starts_with("<<<<<<<"))
            .count();
        self.ensure_cursor_visible();
        self.notify_message(
            if left == 0 {
                "no conflicts left in this file: save, then Space g m s stages it".to_owned()
            } else {
                format!("{left} conflict(s) left in this file")
            },
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\nb\n";

    #[test]
    fn a_block_resolves_to_either_side_or_both() {
        let lines = TEXT.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!((block.start, block.separator, block.end), (1, 3, 5));
        assert_eq!(resolved_text(&lines, block, "ours"), "ours\n");
        assert_eq!(resolved_text(&lines, block, "theirs"), "theirs\n");
        assert_eq!(resolved_text(&lines, block, "both"), "ours\ntheirs\n");
    }

    #[test]
    fn diff3_style_keeps_the_base_out_of_ours() {
        let text = "<<<<<<< HEAD\nours\n||||||| base\nold\n=======\ntheirs\n>>>>>>> other\n";
        let lines = text.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!(resolved_text(&lines, block, "ours"), "ours\n");
    }

    #[test]
    fn the_cursor_picks_its_block_and_an_unterminated_one_is_not_a_block() {
        let text = format!("{TEXT}{TEXT}");
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(conflict_block_at(&lines, 8).unwrap().start, 8);
        assert_eq!(conflict_block_at(&lines, 13).unwrap().start, 8);
        assert_eq!(conflict_block_at(&["<<<<<<< HEAD", "ours"], 0), None);
    }
}
