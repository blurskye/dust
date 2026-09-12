use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{
    state::{EditorMode, EditorState, RegisterShape, ReplaceRecord},
    text::{
        ByteOffset, ByteRange, DisplayWidthPolicy, TextEdit, TextEditSet, display_column_at,
        insertion_at_display_column, next_grapheme_boundary, previous_grapheme_boundary,
    },
    transaction::{EditorOperation, UndoPolicy},
};

use super::{
    super::{CommandError, CommandId, CommandInvocation, CommandOutcome},
    motion::{
        block_start_column, motion_target, operator_range, repeat_visual_ranges, visual_ranges,
    },
    transaction::{
        bool_argument, build_indent, build_insert_transition, build_join_lines, build_toggle_case,
        failed, string_argument, transaction_for,
    },
};

pub(super) fn execute_builtin<E>(
    state: &EditorState,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome<E>, CommandError> {
    let view_id = invocation
        .target
        .view
        .or(state.active_view)
        .ok_or_else(|| CommandError::Unavailable("no active view".into()))?;
    let view = state
        .views
        .get(view_id)
        .ok_or_else(|| CommandError::Unavailable("target view is stale".into()))?;
    let buffer_id = invocation.target.buffer.unwrap_or(view.buffer);
    let buffer = state
        .buffers
        .get(buffer_id)
        .ok_or_else(|| CommandError::Unavailable("target buffer is stale".into()))?;
    let count = invocation.count.map_or(1, |value| value.get());
    let count = usize::try_from(count)
        .map_err(|_| CommandError::InvalidArguments("count is too large".into()))?;
    let mut transaction = transaction_for(view_id, buffer_id, buffer.text.revision());

    match invocation.id.as_str() {
        "view.cursor.move" => {
            let motion = string_argument(invocation, "motion")?;
            let target = motion_target(&buffer.text, view.cursor, motion, count, state.policy)?;
            transaction.undo_policy =
                if matches!(view.mode, EditorMode::Insert | EditorMode::Replace) {
                    UndoPolicy::Boundary
                } else {
                    UndoPolicy::Ignore
                };
            transaction.operations.push(EditorOperation::SetCursor {
                view: view_id,
                position: target,
            });
        }
        "mode.normal" => {
            transaction.undo_policy = UndoPolicy::Boundary;
            if matches!(view.mode, EditorMode::Insert | EditorMode::Replace)
                && view.cursor.get() > 0
            {
                let previous = previous_grapheme_boundary(&buffer.text, view.cursor);
                let crossed_newline = buffer
                    .text
                    .slice(ByteRange::new(previous, view.cursor).map_err(failed)?)
                    .map_err(failed)?
                    == "\n";
                if !crossed_newline {
                    transaction.operations.push(EditorOperation::SetCursor {
                        view: view_id,
                        position: previous,
                    });
                }
            }
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::Normal,
            });
        }
        "mode.replace" => {
            transaction.undo_policy = UndoPolicy::Merge;
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::Replace,
            });
        }
        "mode.visual_char" | "mode.visual_line" | "mode.visual_block" => {
            transaction.undo_policy = UndoPolicy::Ignore;
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: match invocation.id.as_str() {
                    "mode.visual_char" => EditorMode::VisualChar,
                    "mode.visual_line" => EditorMode::VisualLine,
                    _ => EditorMode::VisualBlock,
                },
            });
        }
        "mode.insert" => {
            transaction.undo_policy = UndoPolicy::Merge;
            let placement = string_argument(invocation, "placement")?;
            build_insert_transition(
                &mut transaction,
                &buffer.text,
                view_id,
                buffer_id,
                view.cursor,
                placement,
            )?;
        }
        "edit.insert_text" => {
            transaction.undo_policy = if state.policy.undo_break_on_text_commit {
                UndoPolicy::Record
            } else {
                UndoPolicy::Merge
            };
            let text = string_argument(invocation, "text")?;
            let positions = if view.block_insert.is_empty() {
                vec![view.cursor]
            } else {
                view.block_insert.clone()
            };
            let edits = TextEditSet::new(
                buffer.text.revision(),
                positions
                    .iter()
                    .map(|position| TextEdit::new(ByteRange::empty(*position), text))
                    .collect(),
            );
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
            transaction.operations.push(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(positions[0].get() + text.len()),
            });
        }
        "edit.replace_text" => {
            transaction.undo_policy = if state.policy.undo_break_on_text_commit {
                UndoPolicy::Record
            } else {
                UndoPolicy::Merge
            };
            let replacement = string_argument(invocation, "text")?;
            let mut end = view.cursor;
            for grapheme in replacement.graphemes(true) {
                if grapheme == "\n" {
                    break;
                }
                let next = next_grapheme_boundary(&buffer.text, end);
                if next == end
                    || buffer
                        .text
                        .slice(ByteRange::new(end, next).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                end = next;
            }
            let range = ByteRange::new(view.cursor, end).map_err(failed)?;
            let original = buffer.text.slice(range).map_err(failed)?;
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(
                    buffer.text.revision(),
                    vec![TextEdit::new(range, replacement)],
                ),
            });
            transaction.operations.push(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(view.cursor.get().saturating_add(replacement.len())),
            });
            transaction
                .operations
                .push(EditorOperation::PushReplaceRecord {
                    view: view_id,
                    record: ReplaceRecord {
                        start: view.cursor,
                        original,
                        inserted_len: replacement.len(),
                    },
                });
        }
        "edit.replace_backspace" => {
            transaction.undo_policy = if state.policy.undo_break_on_text_commit {
                UndoPolicy::Record
            } else {
                UndoPolicy::Merge
            };
            let Some(record) = view.replace_history.last() else {
                return Ok(CommandOutcome {
                    transaction: None,
                    effects: Vec::new(),
                    message: None,
                    follow_up: Vec::new(),
                });
            };
            let end = ByteOffset(record.start.get().saturating_add(record.inserted_len));
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(
                    buffer.text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(record.start, end).map_err(failed)?,
                        &record.original,
                    )],
                ),
            });
            transaction.operations.push(EditorOperation::SetCursor {
                view: view_id,
                position: record.start,
            });
            transaction
                .operations
                .push(EditorOperation::PopReplaceRecord { view: view_id });
        }
        "edit.backspace" => {
            transaction.undo_policy = if state.policy.undo_break_on_text_commit {
                UndoPolicy::Record
            } else {
                UndoPolicy::Merge
            };
            let positions = if view.block_insert.is_empty() {
                vec![view.cursor]
            } else {
                view.block_insert.clone()
            };
            let starts = positions
                .iter()
                .map(|position| previous_grapheme_boundary(&buffer.text, *position))
                .collect::<Vec<_>>();
            let edits = starts
                .iter()
                .zip(&positions)
                .filter(|(start, end)| start != end)
                .map(|(start, end)| {
                    ByteRange::new(*start, *end)
                        .map(|range| TextEdit::new(range, ""))
                        .map_err(failed)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !edits.is_empty() {
                if !view.block_insert.is_empty() {
                    transaction
                        .operations
                        .push(EditorOperation::SetBlockInsert {
                            view: view_id,
                            positions: starts.clone(),
                        });
                }
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(buffer.text.revision(), edits),
                });
                transaction.operations.push(EditorOperation::SetCursor {
                    view: view_id,
                    position: starts[0],
                });
            }
        }
        "edit.delete_char" | "edit.substitute" => {
            let mut end = view.cursor;
            for _ in 0..count {
                let next = next_grapheme_boundary(&buffer.text, end);
                if next == end
                    || buffer
                        .text
                        .slice(ByteRange::new(end, next).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                end = next;
            }
            if invocation.id.as_str() == "edit.substitute" {
                transaction.undo_policy = UndoPolicy::Merge;
                transaction.operations.push(EditorOperation::SetMode {
                    view: view_id,
                    mode: EditorMode::Insert,
                });
            }
            if end != view.cursor {
                let text = buffer
                    .text
                    .slice(ByteRange::new(view.cursor, end).map_err(failed)?)
                    .map_err(failed)?;
                transaction.operations.push(EditorOperation::SetRegister {
                    name: invocation.register.unwrap_or('"'),
                    text,
                    shape: RegisterShape::Character,
                });
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(
                        buffer.text.revision(),
                        vec![TextEdit::new(
                            ByteRange::new(view.cursor, end).map_err(failed)?,
                            "",
                        )],
                    ),
                });
            }
        }
        "edit.delete_backward" => {
            let mut start = view.cursor;
            for _ in 0..count {
                let previous = previous_grapheme_boundary(&buffer.text, start);
                if previous == start
                    || buffer
                        .text
                        .slice(ByteRange::new(previous, start).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                start = previous;
            }
            if start != view.cursor {
                let range = ByteRange::new(start, view.cursor).map_err(failed)?;
                let text = buffer.text.slice(range).map_err(failed)?;
                transaction.operations.push(EditorOperation::SetRegister {
                    name: invocation.register.unwrap_or('"'),
                    text,
                    shape: RegisterShape::Character,
                });
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(buffer.text.revision(), vec![TextEdit::new(range, "")]),
                });
                transaction.operations.push(EditorOperation::SetCursor {
                    view: view_id,
                    position: start,
                });
            }
        }
        "edit.replace" => {
            let replacement = string_argument(invocation, "text")?;
            let mut end = view.cursor;
            let mut replaced = 0;
            for _ in 0..count {
                let next = next_grapheme_boundary(&buffer.text, end);
                if next == end
                    || buffer
                        .text
                        .slice(ByteRange::new(end, next).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                end = next;
                replaced += 1;
            }
            if replaced > 0 {
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(
                        buffer.text.revision(),
                        vec![TextEdit::new(
                            ByteRange::new(view.cursor, end).map_err(failed)?,
                            replacement.repeat(replaced),
                        )],
                    ),
                });
                transaction.operations.push(EditorOperation::SetCursor {
                    view: view_id,
                    position: view.cursor,
                });
            }
        }
        "edit.join_lines" => {
            build_join_lines(
                &mut transaction,
                &buffer.text,
                view_id,
                buffer_id,
                view.cursor,
                count,
            )?;
        }
        "edit.toggle_case" => {
            build_toggle_case(
                &mut transaction,
                &buffer.text,
                view_id,
                buffer_id,
                view.cursor,
                count,
            )?;
        }
        "edit.operator" | "edit.repeat_visual" => {
            let operator = string_argument(invocation, "operator")?;
            let (ranges, shape) = if invocation.id.as_str() == "edit.repeat_visual" {
                repeat_visual_ranges(&buffer.text, view, invocation, state.policy)?
            } else {
                let motion = string_argument(invocation, "motion")?;
                let requested_linewise = bool_argument(invocation, "linewise")?;
                if motion == "visual" {
                    visual_ranges(&buffer.text, view, state.policy)?
                } else {
                    (
                        vec![operator_range(
                            &buffer.text,
                            view.cursor,
                            motion,
                            count,
                            requested_linewise,
                            state.policy,
                        )?],
                        if requested_linewise {
                            RegisterShape::Line
                        } else {
                            RegisterShape::Character
                        },
                    )
                }
            };
            let first_range = *ranges
                .first()
                .ok_or_else(|| CommandError::Unavailable("visual block is empty".into()))?;
            if matches!(operator, "indent" | "unindent") {
                let indent_range = ByteRange::new(
                    first_range.start,
                    ranges.last().map_or(first_range.end, |range| range.end),
                )
                .map_err(failed)?;
                build_indent(
                    &mut transaction,
                    &buffer.text,
                    view_id,
                    buffer_id,
                    indent_range,
                    operator == "indent",
                    state.policy,
                )?;
                transaction.operations.push(EditorOperation::SetMode {
                    view: view_id,
                    mode: EditorMode::Normal,
                });
                return Ok(CommandOutcome {
                    transaction: Some(transaction),
                    effects: Vec::new(),
                    message: None,
                    follow_up: Vec::new(),
                });
            }
            let text = ranges
                .iter()
                .map(|range| buffer.text.slice(*range).map_err(failed))
                .collect::<Result<Vec<_>, _>>()?
                .join(if shape == RegisterShape::Block {
                    "\n"
                } else {
                    ""
                });
            transaction.operations.push(EditorOperation::SetRegister {
                name: invocation.register.unwrap_or('"'),
                text,
                shape,
            });
            if operator != "yank" {
                if operator == "change" {
                    transaction.undo_policy = UndoPolicy::Merge;
                }
                if operator == "change" && shape == RegisterShape::Block {
                    transaction
                        .operations
                        .push(EditorOperation::SetBlockInsert {
                            view: view_id,
                            positions: ranges.iter().map(|range| range.start).collect(),
                        });
                }
                let block_column = (operator == "change" && shape == RegisterShape::Block)
                    .then(|| block_start_column(&buffer.text, view, state.policy))
                    .transpose()?;
                let edits = ranges
                    .iter()
                    .copied()
                    .map(|range| {
                        let replacement = block_column.map_or_else(String::new, |column| {
                            if range.is_empty() {
                                let line = buffer.text.byte_to_line(range.start).unwrap_or(0);
                                let line_text = buffer.text.line(line).unwrap_or_default();
                                let content = line_text.strip_suffix('\n').unwrap_or(&line_text);
                                let width_policy = DisplayWidthPolicy {
                                    tab_width: state.policy.tab_width,
                                    ambiguous_wide: state.policy.ambiguous_width_wide,
                                };
                                let (_, padding) =
                                    insertion_at_display_column(content, column, width_policy);
                                " ".repeat(padding)
                            } else {
                                String::new()
                            }
                        });
                        TextEdit::new(range, replacement)
                    })
                    .collect();
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(buffer.text.revision(), edits),
                });
                transaction.operations.push(EditorOperation::SetCursor {
                    view: view_id,
                    position: first_range.start,
                });
            } else {
                transaction.undo_policy = UndoPolicy::Ignore;
            }
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: if operator == "change" {
                    EditorMode::Insert
                } else {
                    EditorMode::Normal
                },
            });
        }
        "edit.put" => {
            let before = bool_argument(invocation, "before")?;
            let register = state
                .registers
                .get(invocation.register.unwrap_or('"'))
                .ok_or_else(|| CommandError::Failed("register is empty".into()))?;
            if register.shape == RegisterShape::Block {
                let cursor_line = buffer.text.byte_to_line(view.cursor).map_err(failed)?;
                let line_start = buffer.text.line_to_byte(cursor_line).map_err(failed)?;
                let source_line = buffer.text.line(cursor_line).map_err(failed)?;
                let source_content = source_line.strip_suffix('\n').unwrap_or(&source_line);
                let width_policy = DisplayWidthPolicy {
                    tab_width: state.policy.tab_width,
                    ambiguous_wide: state.policy.ambiguous_width_wide,
                };
                let cursor_relative = if before {
                    view.cursor.get().saturating_sub(line_start.get())
                } else {
                    next_grapheme_boundary(&buffer.text, view.cursor)
                        .get()
                        .saturating_sub(line_start.get())
                };
                let column = display_column_at(source_content, cursor_relative, width_policy);
                let rows = register.text.split('\n').collect::<Vec<_>>();
                let mut edits = Vec::new();
                for repetition in 0..count {
                    for (row, text) in rows.iter().enumerate() {
                        let target_line = cursor_line
                            .saturating_add(repetition.saturating_mul(rows.len()))
                            .saturating_add(row);
                        if target_line >= buffer.text.len_lines() {
                            break;
                        }
                        let start = buffer.text.line_to_byte(target_line).map_err(failed)?;
                        let line = buffer.text.line(target_line).map_err(failed)?;
                        let content = line.strip_suffix('\n').unwrap_or(&line);
                        let (relative, padding_cells) =
                            insertion_at_display_column(content, column, width_policy);
                        let padding = " ".repeat(padding_cells);
                        edits.push(TextEdit::new(
                            ByteRange::empty(ByteOffset(start.get().saturating_add(relative))),
                            format!("{padding}{text}"),
                        ));
                    }
                }
                transaction.operations.push(EditorOperation::ApplyEdits {
                    buffer: buffer_id,
                    edits: TextEditSet::new(buffer.text.revision(), edits),
                });
                transaction.operations.push(EditorOperation::SetCursor {
                    view: view_id,
                    position: view.cursor,
                });
                return Ok(CommandOutcome {
                    transaction: Some(transaction),
                    effects: Vec::new(),
                    message: None,
                    follow_up: Vec::new(),
                });
            }
            let at = match (register.shape, before) {
                (RegisterShape::Character, true) => view.cursor,
                (RegisterShape::Character, false) => {
                    next_grapheme_boundary(&buffer.text, view.cursor)
                }
                (RegisterShape::Line, true) => {
                    let line = buffer.text.byte_to_line(view.cursor).map_err(failed)?;
                    buffer.text.line_to_byte(line).map_err(failed)?
                }
                (RegisterShape::Line, false) => {
                    let line = buffer.text.byte_to_line(view.cursor).map_err(failed)?;
                    if line + 1 < buffer.text.len_lines() {
                        buffer.text.line_to_byte(line + 1).map_err(failed)?
                    } else {
                        ByteOffset(buffer.text.len_bytes())
                    }
                }
                (RegisterShape::Block, _) => unreachable!("block registers handled above"),
            };
            let text = register.text.repeat(count);
            transaction.operations.push(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits: TextEditSet::new(
                    buffer.text.revision(),
                    vec![TextEdit::new(ByteRange::empty(at), text)],
                ),
            });
            transaction.operations.push(EditorOperation::SetCursor {
                view: view_id,
                position: at,
            });
        }
        "history.undo" => {
            transaction.undo_policy = UndoPolicy::Ignore;
            transaction
                .operations
                .push(EditorOperation::Undo { buffer: buffer_id });
        }
        "history.redo" => {
            transaction.undo_policy = UndoPolicy::Ignore;
            transaction
                .operations
                .push(EditorOperation::Redo { buffer: buffer_id });
        }
        other => {
            return Err(CommandError::Unknown(CommandId::new(other)?));
        }
    }

    Ok(CommandOutcome {
        transaction: Some(transaction),
        effects: Vec::new(),
        message: None,
        follow_up: Vec::new(),
    })
}
