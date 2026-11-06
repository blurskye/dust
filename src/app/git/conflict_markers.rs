//! Accepting one conflict block at a time from its markers, like VS Code's Accept Current,
//! Accept Incoming and Accept Both. The whole-file choices live in `git::conflict`.

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::editor::EditorOperation;

use crate::editor::EditorTransaction;

use crate::app::App;

use crate::editor::Precondition;

use crate::editor::text::TextEdit;

use crate::editor::text::TextEditSet;

use crate::editor::UndoPolicy;

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
        if marker_width(lines[index], '<').is_some()
            && let Some(block) = read_block(lines, index)
        {
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

fn marker_width(line: &str, marker: char) -> Option<usize> {
    let width = line.chars().take_while(|ch| *ch == marker).count();
    // Git's conflict-marker-size attribute can use any positive width.
    (width > 0 && (line.len() == width || line.as_bytes().get(width) == Some(&b' ')))
        .then_some(width)
}

fn read_block(lines: &[&str], start: usize) -> Option<ConflictBlock> {
    let width = marker_width(lines[start], '<')?;
    let mut base = None;
    let mut separator = None;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if marker_width(line, '<') == Some(width) {
            return None;
        }
        if marker_width(line, '|') == Some(width) && separator.is_none() {
            base = Some(index);
        } else if line.len() == width
            && line.bytes().all(|byte| byte == b'=')
            && separator.is_none()
        {
            separator = Some(index);
        } else if marker_width(line, '>') == Some(width) {
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

fn conflict_block_count(lines: &[&str]) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < lines.len() {
        if marker_width(lines[index], '<').is_some()
            && let Some(block) = read_block(lines, index)
        {
            count += 1;
            index = block.end;
        }
        index += 1;
    }
    count
}

/// What a block becomes for `side`: `ours`, `theirs` or `both`.
fn resolved_text(lines: &[&str], block: ConflictBlock, side: &str) -> Result<String, &'static str> {
    let ours = &lines[block.start + 1..block.base.unwrap_or(block.separator)];
    let theirs = &lines[block.separator + 1..block.end];
    let kept = match side {
        "ours" => ours.to_vec(),
        "theirs" => theirs.to_vec(),
        "both" => [ours, theirs].concat(),
        _ => return Err("conflict side must be ours, theirs or both"),
    };
    Ok(kept.iter().map(|line| format!("{line}\n")).collect())
}

impl App {
    pub(in crate::app) fn accept_conflict_block(&mut self, side: &str) {
        let Some(view_id) = self.state.active_view() else {
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
        let replacement = match resolved_text(&lines, block, side) {
            Ok(replacement) => replacement,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let range = text.line_to_byte(block.start).map(|start| {
            let end = text
                .line_to_byte(block.end + 1)
                .unwrap_or(ByteOffset(text.len_bytes()));
            (start, end)
        });
        let Some(range) = range
            .ok()
            .and_then(|(start, end)| ByteRange::new(start, end).ok())
        else {
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
        let remaining = self.state.buffers[buffer_id].text.to_string();
        let left = conflict_block_count(&remaining.lines().collect::<Vec<_>>());
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
    use crate::app::git::conflict_markers::*;

    const TEXT: &str = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\nb\n";

    #[test]
    fn marker_content_and_mismatched_widths_are_preserved() {
        let text = "<<<<<<<< HEAD\nours\n======= section\nmore\n=======\nstill ours\n========\ntheirs\n>>>>>>> other\n>>>>>>>> other\n";
        let lines = text.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!(
            resolved_text(&lines, block, "ours").unwrap(),
            "ours\n======= section\nmore\n=======\nstill ours\n"
        );
        assert_eq!(block.end, 9);
        assert!(conflict_block_at(&["<<<<<<<content", "=======", ">>>>>>>"], 0).is_none());
    }

    #[test]
    fn malformed_candidates_do_not_consume_later_conflicts() {
        for prefix in [
            "<<<<<<< orphan\nunrelated\n",
            "<<<<<<< orphan\n>>>>>>> early\nunrelated\n",
            "<<<<<<< orphan\n=======\nunrelated\n",
        ] {
            let text = format!("{prefix}{TEXT}");
            let lines = text.lines().collect::<Vec<_>>();
            let block = conflict_block_at(&lines, 0).unwrap();
            assert_eq!(block.start, prefix.lines().count() + 1);
            assert_eq!(resolved_text(&lines, block, "theirs").unwrap(), "theirs\n");
            assert_eq!(conflict_block_count(&lines), 1);
        }
    }

    #[test]
    fn shorter_opening_runs_inside_a_conflict_remain_content() {
        let text = "<<<<<<< HEAD\n< input\n<<<<< literal\n=======\n> output\n>>>>>>> other\n";
        let lines = text.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!(
            resolved_text(&lines, block, "ours").unwrap(),
            "< input\n<<<<< literal\n"
        );
        assert_eq!(conflict_block_count(&lines), 1);
    }

    #[test]
    fn git_marker_sizes_are_inferred_and_must_match() {
        for width in [1, 5, 7, 12] {
            let text = format!(
                "{} HEAD\nours\n{} base\nold\n{}\ntheirs\n{} other\n",
                "<".repeat(width),
                "|".repeat(width),
                "=".repeat(width),
                ">".repeat(width),
            );
            let lines = text.lines().collect::<Vec<_>>();
            let block = conflict_block_at(&lines, 0).unwrap();
            assert_eq!(resolved_text(&lines, block, "ours").unwrap(), "ours\n");
            assert_eq!(conflict_block_count(&lines), 1);
        }
        assert!(
            conflict_block_at(
                &["<<<<< HEAD", "ours", "=======", "theirs", ">>>>> other"],
                0
            )
            .is_none()
        );
        assert!(
            conflict_block_at(
                &["<<<<< HEAD", "ours", "=====", "theirs", ">>>>>>> other"],
                0
            )
            .is_none()
        );
    }

    #[test]
    fn remaining_count_ignores_literal_markers_and_incomplete_blocks() {
        let text = format!("<<<<<<<content\n{TEXT}<<<<<<< orphan\n");
        assert_eq!(conflict_block_count(&text.lines().collect::<Vec<_>>()), 1);
        assert_eq!(
            conflict_block_count(&["<<<<<<<content", "<<<<<<< orphan"]),
            0
        );
    }

    #[test]
    fn a_block_resolves_to_either_side_or_both() {
        let lines = TEXT.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!((block.start, block.separator, block.end), (1, 3, 5));
        assert_eq!(resolved_text(&lines, block, "ours").unwrap(), "ours\n");
        assert_eq!(resolved_text(&lines, block, "theirs").unwrap(), "theirs\n");
        assert_eq!(
            resolved_text(&lines, block, "both").unwrap(),
            "ours\ntheirs\n"
        );
        assert!(resolved_text(&lines, block, "our").is_err());
    }

    #[test]
    fn diff3_style_keeps_the_base_out_of_ours() {
        let text = "<<<<<<< HEAD\nours\n||||||| base\nold\n=======\ntheirs\n>>>>>>> other\n";
        let lines = text.lines().collect::<Vec<_>>();
        let block = conflict_block_at(&lines, 0).unwrap();
        assert_eq!(resolved_text(&lines, block, "ours").unwrap(), "ours\n");
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
