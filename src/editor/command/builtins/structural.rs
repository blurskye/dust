//! Incremental syntax selection, definition motions and sibling argument swaps.

use crate::editor::command::builtins::text_object::text_object;
use crate::editor::command::builtins::transaction::failed;
use crate::editor::command::{CommandError, CommandInvocation};
use crate::editor::state::{Buffer, EditorMode, Selection, SelectionKind, View, ViewId};
use crate::editor::text::{
    ByteOffset, ByteRange, TextEdit, TextEditSet, next_grapheme_boundary,
    previous_grapheme_boundary,
};
use crate::editor::{
    EditorOperation, EditorTransaction, SyntaxObject, SyntaxObjectKind, UndoPolicy,
};

pub(super) fn motion_target(
    objects: &[SyntaxObject],
    cursor: ByteOffset,
    motion: &str,
    count: usize,
) -> Option<Result<ByteOffset, CommandError>> {
    let (kind, forward) = match motion {
        "next_function" => (SyntaxObjectKind::Function, true),
        "previous_function" => (SyntaxObjectKind::Function, false),
        "next_class" => (SyntaxObjectKind::Class, true),
        "previous_class" => (SyntaxObjectKind::Class, false),
        _ => return None,
    };
    let mut starts = objects
        .iter()
        .filter(|o| o.kind == kind)
        .map(|o| o.outer.0)
        .filter(|start| {
            if forward {
                *start > cursor.get()
            } else {
                *start < cursor.get()
            }
        })
        .collect::<Vec<_>>();
    starts.sort_unstable();
    starts.dedup();
    if !forward {
        starts.reverse();
    }
    Some(
        starts
            .get(count.saturating_sub(1))
            .copied()
            .map(ByteOffset)
            .ok_or_else(|| {
                CommandError::Unavailable("no matching syntax node in that direction".into())
            }),
    )
}

pub(super) fn apply(
    transaction: &mut EditorTransaction,
    buffer: &Buffer,
    view_id: ViewId,
    view: &View,
    invocation: &CommandInvocation,
    count: usize,
) -> Result<(), CommandError> {
    let objects = buffer.current_syntax_objects();
    let id = invocation.id.as_str();
    if id.starts_with("syntax.argument.swap_") {
        return swap(
            transaction,
            buffer,
            view_id,
            view,
            id.ends_with("next"),
            count,
        );
    }
    transaction.undo_policy = UndoPolicy::Ignore;
    if let Some(motion) = match id {
        "syntax.function.next" => Some("next_function"),
        "syntax.function.previous" => Some("previous_function"),
        "syntax.class.next" => Some("next_class"),
        "syntax.class.previous" => Some("previous_class"),
        _ => None,
    } {
        let position =
            motion_target(objects, view.cursor, motion, count).expect("syntax motion")?;
        transaction.operations.push(EditorOperation::SetCursor {
            view: view_id,
            position,
        });
        return Ok(());
    }
    if !matches!(view.mode, EditorMode::Normal | EditorMode::VisualChar) {
        return Err(CommandError::Unavailable(
            "syntax selection needs Normal or character Visual mode".into(),
        ));
    }
    let mut history = view.syntax_selections.clone();
    let (position, selection) = if id == "syntax.selection.shrink" {
        let mut previous = None;
        for _ in 0..count.min(history.len()) {
            previous = history.pop();
        }
        previous.ok_or_else(|| CommandError::Unavailable("no smaller syntax selection".into()))?
    } else {
        let range = if id == "syntax.selection.grow" {
            let mut range = view.selection.map(|s| {
                (
                    s.anchor.min(s.head).get(),
                    next_grapheme_boundary(&buffer.text, s.anchor.max(s.head)).get(),
                )
            });
            let mut position = view.cursor;
            let mut selection = view.selection;
            for _ in 0..count.min(objects.len()) {
                let node = objects
                    .iter()
                    .filter(|o| o.kind == SyntaxObjectKind::Node)
                    .filter(|o| match range {
                        Some((start, end)) => {
                            o.outer.0 <= start && o.outer.1 >= end && o.outer != (start, end)
                        }
                        None => o.outer.0 <= view.cursor.get() && view.cursor.get() < o.outer.1,
                    })
                    .min_by_key(|o| o.outer.1 - o.outer.0);
                let Some(node) = node else { break };
                history.push((position, selection));
                range = Some(node.outer);
                position = previous_grapheme_boundary(&buffer.text, ByteOffset(node.outer.1));
                selection = Some(Selection {
                    anchor: ByteOffset(node.outer.0),
                    head: position,
                    kind: SelectionKind::Character,
                });
            }
            if history.len() == view.syntax_selections.len() {
                return Err(CommandError::Unavailable(
                    "no larger syntax node (parsing may still be pending)".into(),
                ));
            }
            let (start, end) = range.expect("selected node");
            ByteRange::new(ByteOffset(start), ByteOffset(end)).map_err(failed)?
        } else if let Some(motion) = id.strip_prefix("syntax.select.") {
            history.clear();
            text_object(&buffer.text, objects, view.cursor, motion, count)?
                .ok_or_else(|| CommandError::Unavailable("unknown syntax text object".into()))?
        } else {
            return Err(CommandError::Unavailable("unknown syntax command".into()));
        };
        if range.is_empty() {
            return Err(CommandError::Unavailable("syntax object is empty".into()));
        }
        let head = previous_grapheme_boundary(&buffer.text, range.end);
        (
            head,
            Some(Selection {
                anchor: range.start,
                head,
                kind: SelectionKind::Character,
            }),
        )
    };
    transaction.operations.extend([
        EditorOperation::SetMode {
            view: view_id,
            mode: if selection.is_some() {
                EditorMode::VisualChar
            } else {
                EditorMode::Normal
            },
        },
        EditorOperation::SetSelection {
            view: view_id,
            selection,
        },
        EditorOperation::SetCursor {
            view: view_id,
            position,
        },
        EditorOperation::SetSyntaxSelections {
            view: view_id,
            selections: history,
        },
    ]);
    Ok(())
}

fn swap(
    transaction: &mut EditorTransaction,
    buffer: &Buffer,
    view_id: ViewId,
    view: &View,
    forward: bool,
    count: usize,
) -> Result<(), CommandError> {
    let objects = buffer.current_syntax_objects();
    let current = objects
        .iter()
        .filter(|o| {
            o.kind == SyntaxObjectKind::Argument
                && o.inner.0 <= view.cursor.get()
                && view.cursor.get() < o.inner.1
        })
        .min_by_key(|o| o.inner.1 - o.inner.0)
        .ok_or_else(|| {
            CommandError::Unavailable(
                "no argument under the cursor (parsing may still be pending)".into(),
            )
        })?;
    let mut siblings = objects
        .iter()
        .filter(|o| {
            o.kind == SyntaxObjectKind::Argument && o.parent.is_some() && o.parent == current.parent
        })
        .collect::<Vec<_>>();
    siblings.sort_by_key(|o| o.inner.0);
    let index = siblings
        .iter()
        .position(|o| *o == current)
        .expect("current argument");
    let other = if forward {
        index.checked_add(count)
    } else {
        index.checked_sub(count)
    }
    .and_then(|index| siblings.get(index))
    .copied()
    .ok_or_else(|| CommandError::Unavailable("no sibling argument in that direction".into()))?;
    let range = |object: &SyntaxObject| {
        ByteRange::new(ByteOffset(object.inner.0), ByteOffset(object.inner.1)).map_err(failed)
    };
    let current_range = range(current)?;
    let other_range = range(other)?;
    let current_text = buffer.text.slice(current_range).map_err(failed)?;
    let other_text = buffer.text.slice(other_range).map_err(failed)?;
    let position = if forward {
        other.inner.0 - current_text.len() + other_text.len()
    } else {
        other.inner.0
    };
    transaction.undo_policy = UndoPolicy::Record;
    transaction.operations.extend([
        EditorOperation::ApplyEdits {
            buffer: view.buffer,
            edits: TextEditSet::new(
                buffer.text.revision(),
                vec![
                    TextEdit::new(current_range, other_text),
                    TextEdit::new(other_range, current_text),
                ],
            ),
        },
        EditorOperation::SetMode {
            view: view_id,
            mode: EditorMode::Normal,
        },
        EditorOperation::SetCursor {
            view: view_id,
            position: ByteOffset(position),
        },
    ]);
    Ok(())
}
