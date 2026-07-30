use std::sync::atomic::{AtomicU64, Ordering};

use unicode_segmentation::UnicodeSegmentation;

use crate::document::{
    ByteOffset, ByteRange, DisplayWidthPolicy, TextBuffer, TextEdit, TextEditSet,
    byte_range_for_display_columns, display_column_at, insertion_at_display_column,
    next_grapheme_boundary, previous_grapheme_boundary,
};

use crate::editing::{
    ArgumentSchema, ArgumentType, CommandError, CommandInvocation, CommandOutcome, CommandRegistry,
    CommandSpec, ContextRequirement, EditorMode, EditorOperation, EditorState, EditorTransaction,
    Precondition, RegisterShape, SelectionKind, TransactionId, UndoPolicy, View, ViewId,
};

static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub fn register_builtin_commands<E: 'static>(
    registry: &mut CommandRegistry<E>,
) -> Result<(), CommandError> {
    register(
        registry,
        "view.cursor.move",
        "Move cursor",
        ContextRequirement::View,
        ArgumentSchema::default().field("motion", ArgumentType::String, true),
    )?;
    register(
        registry,
        "mode.insert",
        "Enter Insert mode",
        ContextRequirement::View,
        ArgumentSchema::default().field("placement", ArgumentType::String, true),
    )?;
    for (id, title) in [
        ("mode.normal", "Enter Normal mode"),
        ("mode.replace", "Enter Replace mode"),
        ("mode.visual_char", "Enter character Visual mode"),
        ("mode.visual_line", "Enter line Visual mode"),
        ("mode.visual_block", "Enter block Visual mode"),
        ("edit.delete_char", "Delete character"),
        (
            "edit.delete_backward",
            "Delete previous character in Normal mode",
        ),
        ("edit.substitute", "Substitute characters"),
        ("edit.join_lines", "Join lines"),
        ("edit.toggle_case", "Toggle character case"),
        ("edit.backspace", "Delete previous character"),
        (
            "edit.replace_backspace",
            "Restore previous Replace-mode text",
        ),
        ("history.undo", "Undo"),
        ("history.redo", "Redo"),
    ] {
        register(
            registry,
            id,
            title,
            ContextRequirement::View,
            ArgumentSchema::default(),
        )?;
    }
    register(
        registry,
        "edit.replace",
        "Replace characters",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.insert_text",
        "Insert text",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.replace_text",
        "Replace text in Replace mode",
        ContextRequirement::View,
        ArgumentSchema::default().field("text", ArgumentType::String, true),
    )?;
    register(
        registry,
        "edit.operator",
        "Apply operator",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("operator", ArgumentType::String, true)
            .field("motion", ArgumentType::String, true)
            .field("linewise", ArgumentType::Bool, true),
    )?;
    register(
        registry,
        "edit.repeat_visual",
        "Repeat normalized Visual change",
        ContextRequirement::View,
        ArgumentSchema::default()
            .field("operator", ArgumentType::String, true)
            .field("shape", ArgumentType::String, true)
            .field("width", ArgumentType::Integer, true)
            .field("height", ArgumentType::Integer, true),
    )?;
    register(
        registry,
        "edit.put",
        "Put register",
        ContextRequirement::View,
        ArgumentSchema::default().field("before", ArgumentType::Bool, true),
    )?;
    Ok(())
}

fn register<E: 'static>(
    registry: &mut CommandRegistry<E>,
    id: &str,
    title: &str,
    context: ContextRequirement,
    arguments: ArgumentSchema,
) -> Result<(), CommandError> {
    let mut spec = CommandSpec::builtin(id, title)?;
    spec.context = context;
    spec.arguments = arguments;
    registry.register(spec, execute_builtin::<E>)
}

fn execute_builtin<E>(
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
                    record: crate::editing::ReplaceRecord {
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
            return Err(CommandError::Unknown(crate::editing::CommandId::new(
                other,
            )?));
        }
    }

    Ok(CommandOutcome {
        transaction: Some(transaction),
        effects: Vec::new(),
        message: None,
        follow_up: Vec::new(),
    })
}

fn transaction_for(
    view: ViewId,
    buffer: crate::editing::BufferId,
    revision: crate::document::BufferRevision,
) -> EditorTransaction {
    EditorTransaction::new(TransactionId(
        NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed),
    ))
    .with_precondition(Precondition::ViewDisplays { view, buffer })
    .with_precondition(Precondition::BufferRevision { buffer, revision })
}

fn string_argument<'a>(
    invocation: &'a CommandInvocation,
    name: &str,
) -> Result<&'a str, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_str()
}

fn integer_argument(invocation: &CommandInvocation, name: &str) -> Result<i64, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_integer()
}

fn bool_argument(invocation: &CommandInvocation, name: &str) -> Result<bool, CommandError> {
    invocation
        .arguments
        .get(name)
        .ok_or_else(|| CommandError::InvalidArguments(format!("missing `{name}`")))?
        .as_bool()
}

fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::Failed(error.to_string())
}

fn build_insert_transition(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: crate::editing::BufferId,
    cursor: ByteOffset,
    placement: &str,
) -> Result<(), CommandError> {
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let line_start = text.line_to_byte(line).map_err(failed)?;
    let content_end = line_content_end(text, line)?;
    let mut target = cursor;
    let mut insertion = None;
    match placement {
        "before" => {}
        "after" => target = next_grapheme_boundary(text, cursor).min(content_end),
        "line_start" => target = first_nonblank(text, line)?,
        "line_end" => target = content_end,
        "open_below" => {
            target = content_end;
            insertion = Some((target, "\n"));
            target = ByteOffset(target.get() + 1);
        }
        "open_above" => {
            target = line_start;
            insertion = Some((target, "\n"));
        }
        _ => {
            return Err(CommandError::InvalidArguments(
                "unknown insert placement".into(),
            ));
        }
    }
    if let Some((at, inserted)) = insertion {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(
                text.revision(),
                vec![TextEdit::new(ByteRange::empty(at), inserted)],
            ),
        });
    }
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: target,
    });
    transaction.operations.push(EditorOperation::SetMode {
        view,
        mode: EditorMode::Insert,
    });
    Ok(())
}

fn build_join_lines(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: crate::editing::BufferId,
    cursor: ByteOffset,
    count: usize,
) -> Result<(), CommandError> {
    let first_line = text.byte_to_line(cursor).map_err(failed)?;
    let last_line = first_line
        .saturating_add(count)
        .min(text.len_lines().saturating_sub(1));
    if last_line == first_line {
        return Ok(());
    }
    let start = text.line_to_byte(first_line).map_err(failed)?;
    let end = line_content_end(text, last_line)?;
    let mut joined = String::new();
    for line in first_line..=last_line {
        let value = text.line(line).map_err(failed)?;
        let content = value.strip_suffix('\n').unwrap_or(&value);
        if line == first_line {
            joined.push_str(content.trim_end());
        } else {
            let content = content.trim();
            if !joined.is_empty() && !content.is_empty() {
                joined.push(' ');
            }
            joined.push_str(content);
        }
    }
    let target = ByteOffset(cursor.get().min(start.get() + joined.len()));
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            vec![TextEdit::new(
                ByteRange::new(start, end).map_err(failed)?,
                joined,
            )],
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: target,
    });
    Ok(())
}

fn build_toggle_case(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: crate::editing::BufferId,
    cursor: ByteOffset,
    count: usize,
) -> Result<(), CommandError> {
    let mut end = cursor;
    let mut replacement = String::new();
    for _ in 0..count {
        let next = next_grapheme_boundary(text, end);
        if next == end {
            break;
        }
        let grapheme = text
            .slice(ByteRange::new(end, next).map_err(failed)?)
            .map_err(failed)?;
        if grapheme == "\n" {
            break;
        }
        for character in grapheme.chars() {
            if character.is_lowercase() {
                replacement.extend(character.to_uppercase());
            } else if character.is_uppercase() {
                replacement.extend(character.to_lowercase());
            } else {
                replacement.push(character);
            }
        }
        end = next;
    }
    if end != cursor {
        transaction.operations.push(EditorOperation::ApplyEdits {
            buffer,
            edits: TextEditSet::new(
                text.revision(),
                vec![TextEdit::new(
                    ByteRange::new(cursor, end).map_err(failed)?,
                    replacement,
                )],
            ),
        });
        transaction.operations.push(EditorOperation::SetCursor {
            view,
            position: cursor,
        });
    }
    Ok(())
}

fn build_indent(
    transaction: &mut EditorTransaction,
    text: &TextBuffer,
    view: ViewId,
    buffer: crate::editing::BufferId,
    range: ByteRange,
    indent: bool,
    policy: crate::editing::EditingPolicy,
) -> Result<(), CommandError> {
    let first_line = text.byte_to_line(range.start).map_err(failed)?;
    let final_byte = range.end.get().saturating_sub(1).max(range.start.get());
    let last_line = text.byte_to_line(ByteOffset(final_byte)).map_err(failed)?;
    let start = text.line_to_byte(first_line).map_err(failed)?;
    let end = if last_line + 1 < text.len_lines() {
        text.line_to_byte(last_line + 1).map_err(failed)?
    } else {
        ByteOffset(text.len_bytes())
    };
    let source = text
        .slice(ByteRange::new(start, end).map_err(failed)?)
        .map_err(failed)?;
    let mut replacement = String::new();
    for line in source.split_inclusive('\n') {
        if indent {
            if policy.expand_tabs {
                replacement.push_str(&" ".repeat(policy.indent_width));
            } else {
                replacement.push('\t');
            }
            replacement.push_str(line);
        } else if let Some(rest) = line.strip_prefix('\t') {
            replacement.push_str(rest);
        } else {
            let spaces = line
                .bytes()
                .take_while(|byte| *byte == b' ')
                .take(policy.indent_width)
                .count();
            replacement.push_str(&line[spaces..]);
        }
    }
    transaction.operations.push(EditorOperation::ApplyEdits {
        buffer,
        edits: TextEditSet::new(
            text.revision(),
            vec![TextEdit::new(
                ByteRange::new(start, end).map_err(failed)?,
                replacement,
            )],
        ),
    });
    transaction.operations.push(EditorOperation::SetCursor {
        view,
        position: start,
    });
    Ok(())
}

fn motion_target(
    text: &TextBuffer,
    cursor: ByteOffset,
    motion: &str,
    count: usize,
    policy: crate::editing::EditingPolicy,
) -> Result<ByteOffset, CommandError> {
    let mut target = cursor;
    match motion {
        "left" => {
            for _ in 0..count {
                let previous = previous_grapheme_boundary(text, target);
                if text
                    .slice(ByteRange::new(previous, target).map_err(failed)?)
                    .map_err(failed)?
                    == "\n"
                {
                    break;
                }
                target = previous;
            }
        }
        "right" => {
            for _ in 0..count {
                let next = next_grapheme_boundary(text, target);
                if next == target
                    || text
                        .slice(ByteRange::new(target, next).map_err(failed)?)
                        .map_err(failed)?
                        == "\n"
                {
                    break;
                }
                target = next;
            }
        }
        "up" | "down" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            let start = text.line_to_byte(line).map_err(failed)?;
            let source_line = text.line(line).map_err(failed)?;
            let source_content = source_line.strip_suffix('\n').unwrap_or(&source_line);
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let column = display_column_at(
                source_content,
                cursor.get().saturating_sub(start.get()),
                width_policy,
            );
            let destination = if motion == "up" {
                line.saturating_sub(count)
            } else {
                line.saturating_add(count)
                    .min(text.len_lines().saturating_sub(1))
            };
            let destination_start = text.line_to_byte(destination).map_err(failed)?;
            let destination_line = text.line(destination).map_err(failed)?;
            let destination_content = destination_line
                .strip_suffix('\n')
                .unwrap_or(&destination_line);
            let (relative, _) =
                insertion_at_display_column(destination_content, column, width_policy);
            target = ByteOffset(destination_start.get().saturating_add(relative));
        }
        "line_start" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            target = text.line_to_byte(line).map_err(failed)?;
        }
        "first_nonblank" => {
            target = first_nonblank(text, text.byte_to_line(cursor).map_err(failed)?)?;
        }
        "line_end" => {
            let line = text.byte_to_line(cursor).map_err(failed)?;
            let end = line_content_end(text, line)?;
            target = if end == text.line_to_byte(line).map_err(failed)? {
                end
            } else {
                previous_grapheme_boundary(text, end)
            };
        }
        "word_forward" => {
            for _ in 0..count {
                target = word_forward(text, target);
            }
        }
        "word_backward" => {
            for _ in 0..count {
                target = word_backward(text, target);
            }
        }
        "big_word_forward" => {
            for _ in 0..count {
                target = big_word_forward(text, target);
            }
        }
        "big_word_backward" => {
            for _ in 0..count {
                target = big_word_backward(text, target);
            }
        }
        "word_end" => {
            for index in 0..count {
                if index > 0 {
                    target = next_grapheme_boundary(text, target);
                }
                target = word_end(text, target);
            }
        }
        "big_word_end" => {
            for index in 0..count {
                if index > 0 {
                    target = next_grapheme_boundary(text, target);
                }
                target = big_word_end(text, target);
            }
        }
        "matching_pair" => target = matching_pair(text, target)?,
        "document_start" => target = ByteOffset(0),
        "document_end" => {
            if text.is_empty() {
                target = ByteOffset(0);
            } else {
                target = previous_grapheme_boundary(text, ByteOffset(text.len_bytes()));
            }
        }
        _ => {
            return Err(CommandError::InvalidArguments(format!(
                "unknown motion `{motion}`"
            )));
        }
    }
    Ok(target)
}

fn block_start_column(
    text: &TextBuffer,
    view: &View,
    policy: crate::editing::EditingPolicy,
) -> Result<usize, CommandError> {
    let width_policy = DisplayWidthPolicy {
        tab_width: policy.tab_width,
        ambiguous_wide: policy.ambiguous_width_wide,
    };
    let column_at = |offset: ByteOffset| -> Result<usize, CommandError> {
        let line = text.byte_to_line(offset).map_err(failed)?;
        let start = text.line_to_byte(line).map_err(failed)?;
        let line_text = text.line(line).map_err(failed)?;
        let content = line_text.strip_suffix('\n').unwrap_or(&line_text);
        Ok(display_column_at(
            content,
            offset.get().saturating_sub(start.get()),
            width_policy,
        ))
    };
    if let Some(selection) = view
        .selection
        .filter(|selection| selection.kind == SelectionKind::Block)
    {
        Ok(column_at(selection.anchor)?.min(column_at(selection.head)?))
    } else {
        column_at(view.cursor)
    }
}

fn repeat_visual_ranges(
    text: &TextBuffer,
    view: &View,
    invocation: &CommandInvocation,
    policy: crate::editing::EditingPolicy,
) -> Result<(Vec<ByteRange>, RegisterShape), CommandError> {
    let shape = string_argument(invocation, "shape")?;
    let width = usize::try_from(integer_argument(invocation, "width")?)
        .map_err(|_| CommandError::InvalidArguments("visual repeat width is invalid".into()))?;
    let height = usize::try_from(integer_argument(invocation, "height")?)
        .map_err(|_| CommandError::InvalidArguments("visual repeat height is invalid".into()))?;
    if width == 0 || height == 0 {
        return Err(CommandError::InvalidArguments(
            "visual repeat dimensions must be positive".into(),
        ));
    }
    match shape {
        "character" => {
            let mut end = view.cursor;
            for _ in 0..width {
                let next = next_grapheme_boundary(text, end);
                if next == end {
                    break;
                }
                end = next;
            }
            Ok((
                vec![ByteRange::new(view.cursor, end).map_err(failed)?],
                RegisterShape::Character,
            ))
        }
        "line" => {
            let first_line = text.byte_to_line(view.cursor).map_err(failed)?;
            let start = text.line_to_byte(first_line).map_err(failed)?;
            let after = first_line.saturating_add(height);
            let end = if after < text.len_lines() {
                text.line_to_byte(after).map_err(failed)?
            } else {
                ByteOffset(text.len_bytes())
            };
            Ok((
                vec![ByteRange::new(start, end).map_err(failed)?],
                RegisterShape::Line,
            ))
        }
        "block" => {
            let first_line = text.byte_to_line(view.cursor).map_err(failed)?;
            let line_start = text.line_to_byte(first_line).map_err(failed)?;
            let line = text.line(first_line).map_err(failed)?;
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let first_column = display_column_at(
                content,
                view.cursor.get().saturating_sub(line_start.get()),
                width_policy,
            );
            let last_column = first_column.saturating_add(width.saturating_sub(1));
            let mut ranges = Vec::new();
            for line_index in first_line..first_line.saturating_add(height).min(text.len_lines()) {
                let start = text.line_to_byte(line_index).map_err(failed)?;
                let line = text.line(line_index).map_err(failed)?;
                let content = line.strip_suffix('\n').unwrap_or(&line);
                if let Some((relative_start, relative_end)) =
                    byte_range_for_display_columns(content, first_column, last_column, width_policy)
                {
                    ranges.push(
                        ByteRange::new(
                            ByteOffset(start.get().saturating_add(relative_start)),
                            ByteOffset(start.get().saturating_add(relative_end)),
                        )
                        .map_err(failed)?,
                    );
                } else {
                    ranges.push(ByteRange::empty(ByteOffset(
                        start.get().saturating_add(content.len()),
                    )));
                }
            }
            Ok((ranges, RegisterShape::Block))
        }
        _ => Err(CommandError::InvalidArguments(format!(
            "unknown visual repeat shape `{shape}`"
        ))),
    }
}

fn visual_ranges(
    text: &TextBuffer,
    view: &View,
    policy: crate::editing::EditingPolicy,
) -> Result<(Vec<ByteRange>, RegisterShape), CommandError> {
    let selection = view
        .selection
        .ok_or_else(|| CommandError::Unavailable("visual selection is missing".into()))?;
    let low = selection.anchor.min(selection.head);
    let high = selection.anchor.max(selection.head);
    match selection.kind {
        SelectionKind::Character => {
            let end = next_grapheme_boundary(text, high);
            Ok((
                vec![ByteRange::new(low, end).map_err(failed)?],
                RegisterShape::Character,
            ))
        }
        SelectionKind::Line => {
            let first_line = text.byte_to_line(low).map_err(failed)?;
            let last_line = text.byte_to_line(high).map_err(failed)?;
            let start = text.line_to_byte(first_line).map_err(failed)?;
            let end = if last_line + 1 < text.len_lines() {
                text.line_to_byte(last_line + 1).map_err(failed)?
            } else {
                ByteOffset(text.len_bytes())
            };
            Ok((
                vec![ByteRange::new(start, end).map_err(failed)?],
                RegisterShape::Line,
            ))
        }
        SelectionKind::Block => {
            let anchor_line = text.byte_to_line(selection.anchor).map_err(failed)?;
            let head_line = text.byte_to_line(selection.head).map_err(failed)?;
            let anchor_start = text.line_to_byte(anchor_line).map_err(failed)?;
            let head_start = text.line_to_byte(head_line).map_err(failed)?;
            let width_policy = DisplayWidthPolicy {
                tab_width: policy.tab_width,
                ambiguous_wide: policy.ambiguous_width_wide,
            };
            let anchor_text = text.line(anchor_line).map_err(failed)?;
            let anchor_content = anchor_text.strip_suffix('\n').unwrap_or(&anchor_text);
            let head_text = text.line(head_line).map_err(failed)?;
            let head_content = head_text.strip_suffix('\n').unwrap_or(&head_text);
            let anchor_column = display_column_at(
                anchor_content,
                selection.anchor.get().saturating_sub(anchor_start.get()),
                width_policy,
            );
            let head_column = display_column_at(
                head_content,
                selection.head.get().saturating_sub(head_start.get()),
                width_policy,
            );
            let first_column = anchor_column.min(head_column);
            let last_column = anchor_column.max(head_column);
            let mut ranges = Vec::new();
            for line_index in anchor_line.min(head_line)..=anchor_line.max(head_line) {
                let line_start = text.line_to_byte(line_index).map_err(failed)?;
                let line = text.line(line_index).map_err(failed)?;
                let content = line.strip_suffix('\n').unwrap_or(&line);
                if let Some((relative_start, relative_end)) =
                    byte_range_for_display_columns(content, first_column, last_column, width_policy)
                {
                    ranges.push(
                        ByteRange::new(
                            ByteOffset(line_start.get().saturating_add(relative_start)),
                            ByteOffset(line_start.get().saturating_add(relative_end)),
                        )
                        .map_err(failed)?,
                    );
                } else {
                    ranges.push(ByteRange::empty(ByteOffset(
                        line_start.get().saturating_add(content.len()),
                    )));
                }
            }
            Ok((ranges, RegisterShape::Block))
        }
    }
}

fn operator_range(
    text: &TextBuffer,
    cursor: ByteOffset,
    motion: &str,
    count: usize,
    linewise: bool,
    policy: crate::editing::EditingPolicy,
) -> Result<ByteRange, CommandError> {
    if linewise {
        let line = text.byte_to_line(cursor).map_err(failed)?;
        let start = text.line_to_byte(line).map_err(failed)?;
        let after = line.saturating_add(count);
        let end = if after < text.len_lines() {
            text.line_to_byte(after).map_err(failed)?
        } else {
            ByteOffset(text.len_bytes())
        };
        return ByteRange::new(start, end).map_err(failed);
    }
    let target = motion_target(text, cursor, motion, count, policy)?;
    let (start, mut end) = if target < cursor {
        (target, cursor)
    } else {
        (cursor, target)
    };
    if target >= cursor
        && matches!(
            motion,
            "right" | "word_end" | "big_word_end" | "line_end" | "matching_pair"
        )
    {
        end = next_grapheme_boundary(text, end);
    }
    ByteRange::new(start, end).map_err(failed)
}

fn line_content_end(text: &TextBuffer, line: usize) -> Result<ByteOffset, CommandError> {
    let start = text.line_to_byte(line).map_err(failed)?;
    let value = text.line(line).map_err(failed)?;
    let content_len = value.strip_suffix('\n').map_or(value.len(), str::len);
    Ok(ByteOffset(start.get() + content_len))
}

fn first_nonblank(text: &TextBuffer, line: usize) -> Result<ByteOffset, CommandError> {
    let start = text.line_to_byte(line).map_err(failed)?;
    let value = text.line(line).map_err(failed)?;
    let relative = value
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(0, |(index, _)| index);
    Ok(ByteOffset(start.get() + relative))
}

fn word_class(character: char) -> u8 {
    if character.is_whitespace() {
        0
    } else if character.is_alphanumeric() || character == '_' {
        1
    } else {
        2
    }
}

fn word_forward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    if cursor.get() >= source.len() {
        return cursor;
    }
    let mut iter = source[cursor.get()..].char_indices();
    let current = iter
        .next()
        .map_or(0, |(_, character)| word_class(character));
    let mut seen_other = false;
    for (relative, character) in iter {
        let class = word_class(character);
        if !seen_other && class != current {
            seen_other = true;
        }
        if seen_other && class != 0 {
            return ByteOffset(cursor.get() + relative);
        }
    }
    ByteOffset(source.len())
}

fn big_word_forward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let starts_in_space = source[cursor.get()..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace);
    let mut separator_seen = starts_in_space;
    for (relative, character) in source[cursor.get()..].char_indices() {
        if character.is_whitespace() {
            separator_seen = true;
        } else if separator_seen {
            return ByteOffset(cursor.get() + relative);
        }
    }
    ByteOffset(source.len())
}

fn big_word_backward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut target = 0;
    let mut seen_nonspace = false;
    for (index, character) in source[..cursor.get()].char_indices().rev() {
        if character.is_whitespace() {
            if seen_nonspace {
                break;
            }
        } else {
            target = index;
            seen_nonspace = true;
        }
    }
    ByteOffset(target)
}

fn word_backward(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let before = &source[..cursor.get()];
    let mut iter = before.char_indices().rev();
    let Some((mut target, mut character)) = iter.next() else {
        return ByteOffset(0);
    };
    while character.is_whitespace() {
        let Some((index, next)) = iter.next() else {
            return ByteOffset(0);
        };
        target = index;
        character = next;
    }
    let class = word_class(character);
    for (index, previous) in iter {
        if word_class(previous) != class {
            break;
        }
        target = index;
    }
    ByteOffset(target)
}

fn big_word_end(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut end = cursor.get();
    let mut seen_nonspace = false;
    for (relative, character) in source[cursor.get()..].char_indices() {
        if character.is_whitespace() {
            if seen_nonspace {
                break;
            }
        } else {
            seen_nonspace = true;
            end = cursor.get() + relative;
        }
    }
    ByteOffset(end)
}

fn matching_pair(text: &TextBuffer, cursor: ByteOffset) -> Result<ByteOffset, CommandError> {
    let source = text.to_string();
    let line = text.byte_to_line(cursor).map_err(failed)?;
    let line_end = line_content_end(text, line)?.get();
    let (at, character) = source[cursor.get()..line_end]
        .char_indices()
        .find_map(|(relative, character)| {
            "()[]{}"
                .contains(character)
                .then_some((cursor.get() + relative, character))
        })
        .ok_or_else(|| CommandError::Unavailable("no matching pair on line".into()))?;
    let (mate, forward) = match character {
        '(' => (')', true),
        '[' => (']', true),
        '{' => ('}', true),
        ')' => ('(', false),
        ']' => ('[', false),
        '}' => ('{', false),
        _ => unreachable!(),
    };
    let mut depth = 0_usize;
    if forward {
        for (relative, candidate) in source[at..].char_indices() {
            if candidate == character {
                depth += 1;
            } else if candidate == mate {
                depth -= 1;
                if depth == 0 {
                    return Ok(ByteOffset(at + relative));
                }
            }
        }
    } else {
        for (index, candidate) in source[..=at].char_indices().rev() {
            if candidate == character {
                depth += 1;
            } else if candidate == mate {
                depth -= 1;
                if depth == 0 {
                    return Ok(ByteOffset(index));
                }
            }
        }
    }
    Err(CommandError::Unavailable("matching pair not found".into()))
}

fn word_end(text: &TextBuffer, cursor: ByteOffset) -> ByteOffset {
    let source = text.to_string();
    let mut iter = source[cursor.get()..].char_indices().peekable();
    while iter
        .peek()
        .is_some_and(|(_, character)| character.is_whitespace())
    {
        iter.next();
    }
    let Some((first, character)) = iter.next() else {
        return ByteOffset(source.len());
    };
    let class = word_class(character);
    let mut end = cursor.get() + first;
    for (relative, next) in iter {
        if word_class(next) != class {
            break;
        }
        end = cursor.get() + relative;
    }
    ByteOffset(end)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::document::TextBuffer;

    use crate::editing::{
        CommandInvocation, CommandRegistry, CommandValue, EditorState, InvocationSource,
        register_builtin_commands,
    };

    #[test]
    fn movement_and_edit_commands_apply_through_registry_transactions() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (_, view) = state.open_text(TextBuffer::from_text("one two"), None);
        let mut registry = CommandRegistry::<()>::default();
        register_builtin_commands(&mut registry).expect("builtins");
        let mut move_word = CommandInvocation::new("view.cursor.move", InvocationSource::Test)
            .expect("invocation")
            .with_argument("motion", CommandValue::String("word_forward".into()));
        move_word.target.view = Some(view);
        let outcome = registry.execute(&state, &move_word).expect("execute");
        state
            .apply_transaction(outcome.transaction.as_ref().expect("transaction"))
            .expect("apply");
        assert_eq!(state.views[view].cursor.get(), 4);

        state.policy = crate::editing::EditingPolicy {
            indent_width: 2,
            expand_tabs: false,
            ..crate::editing::EditingPolicy::default()
        };
        let mut indent = CommandInvocation::new("edit.operator", InvocationSource::Test)
            .expect("indent")
            .with_argument("operator", CommandValue::String("indent".into()))
            .with_argument("motion", CommandValue::String("line".into()))
            .with_argument("linewise", CommandValue::Bool(true));
        indent.target.view = Some(view);
        let outcome = registry
            .execute(&state, &indent)
            .expect("configured indent");
        state
            .apply_transaction(outcome.transaction.as_ref().expect("transaction"))
            .expect("apply");
        assert!(
            state.buffers[state.views[view].buffer]
                .text
                .to_string()
                .starts_with('\t')
        );
    }
}
