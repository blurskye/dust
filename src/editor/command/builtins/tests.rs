//! Tests for built-in editing commands.

use std::path::PathBuf;

use crate::editor::text::TextBuffer;

use crate::editor::CommandInvocation;

use crate::editor::CommandRegistry;

use crate::editor::CommandValue;

use crate::editor::EditorState;

use crate::editor::InvocationSource;

use crate::editor::register_builtin_commands;

#[test]
fn checked_edit_rejects_stale_ranges_utf8_splits_and_invalid_cursors_atomically() {
    let (mut state, buffer, view) = editing("λx", 0);
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    for (revision, start, end, cursor) in [(1, 0, 2, 0), (0, 1, 2, 0), (0, 2, 1, 0), (0, 0, 2, 99)]
    {
        let invocation = CommandInvocation::new("edit.apply", InvocationSource::Test)
            .unwrap()
            .with_argument("revision", CommandValue::Integer(revision))
            .with_argument("start", CommandValue::Integer(start))
            .with_argument("end", CommandValue::Integer(end))
            .with_argument("cursor", CommandValue::Integer(cursor))
            .with_argument("text", CommandValue::String("z".into()));
        match registry.execute(&state, &invocation) {
            Err(_) => {}
            Ok(outcome) => assert!(
                state
                    .apply_transaction(&outcome.transaction.unwrap())
                    .is_err()
            ),
        }
        assert_eq!(state.buffers[buffer].text.to_string(), "λx");
        assert_eq!(state.views[view].cursor.0, 0);
    }
}

#[test]
fn checked_operator_range_uses_native_registers_and_rejects_stale_revisions() {
    let (mut state, buffer, _) = editing("one two", 0);
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    let mut invocation = CommandInvocation::new("edit.operator", InvocationSource::Test)
        .unwrap()
        .with_argument("operator", CommandValue::String("delete".into()))
        .with_argument("motion", CommandValue::String("guest_object".into()))
        .with_argument("linewise", CommandValue::Bool(false))
        .with_argument("revision", CommandValue::Integer(0))
        .with_argument("start", CommandValue::Integer(4))
        .with_argument("end", CommandValue::Integer(7));
    invocation.register = Some('a');
    let outcome = registry.execute(&state, &invocation).unwrap();
    state
        .apply_transaction(&outcome.transaction.unwrap())
        .unwrap();
    assert_eq!(state.buffers[buffer].text.to_string(), "one ");
    assert_eq!(state.registers.get('a').unwrap().text, "two");
    assert!(registry.execute(&state, &invocation).is_err());
    apply(&mut state, "history.undo", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "one two");
}

#[test]
fn movement_and_edit_commands_apply_through_registry_transactions() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state
        .open_text(TextBuffer::from_text("one two"), None)
        .unwrap();
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

    state.policy = crate::editor::EditingPolicy {
        tab_width: 2,
        indent_width: 2,
        expand_tabs: false,
        ..crate::editor::EditingPolicy::default()
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
    assert_eq!(
        state.buffers[state.views[view].buffer].text.to_string(),
        "\tone two"
    );
}

#[test]
fn explicit_buffer_target_must_match_the_view_before_moving() {
    let (mut state, _, view) = editing("abcdef", 4);
    let other = state.buffers.insert(crate::editor::state::Buffer::new(
        TextBuffer::from_text("x"),
        None,
    ));
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    let mut invocation = CommandInvocation::new("view.cursor.move", InvocationSource::Test)
        .unwrap()
        .with_argument("motion", CommandValue::String("right".into()));
    invocation.target.view = Some(view);
    invocation.target.buffer = Some(other);
    assert!(matches!(registry.execute(&state, &invocation),
        Err(crate::editor::CommandError::Unavailable(message)) if message.contains("different buffer")));
    assert_eq!(state.views[view].cursor.get(), 4);
    assert_eq!(state.buffers[other].text.to_string(), "x");
}

fn apply(state: &mut EditorState, id: &str, arguments: &[(&str, &str)], count: u64) {
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    let mut invocation = CommandInvocation::new(id, InvocationSource::Test).unwrap();
    invocation.count = std::num::NonZeroU64::new(count);
    for (name, value) in arguments {
        invocation = invocation.with_argument(
            name,
            match *value {
                "true" => CommandValue::Bool(true),
                "false" => CommandValue::Bool(false),
                value => CommandValue::String(value.into()),
            },
        );
    }
    let outcome = registry.execute(state, &invocation).unwrap();
    if let Some(transaction) = outcome.transaction {
        state.apply_transaction(&transaction).unwrap();
    }
}

fn editing(
    text: &str,
    cursor: usize,
) -> (EditorState, crate::editor::BufferId, crate::editor::ViewId) {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(TextBuffer::from_text(text), None).unwrap();
    state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
    (state, buffer, view)
}

#[test]
fn linewise_put_separates_an_unterminated_last_line() {
    use crate::editor::RegisterShape;
    use crate::editor::RegisterValue;
    let (mut state, buffer, view) = editing("one\ntwo", 4);
    state.registers.set(
        '"',
        RegisterValue {
            text: "two".into(),
            shape: RegisterShape::Line,
        },
        true,
        true,
    );
    apply(&mut state, "edit.put", &[("before", "false")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "one\ntwo\ntwo\n");
    assert_eq!(state.views[view].cursor.get(), 8);
}

#[test]
fn characterwise_put_after_stays_on_an_empty_line() {
    use crate::editor::RegisterShape;
    use crate::editor::RegisterValue;
    for (text, cursor, expected) in [
        ("\nabc", 0, "X\nabc"),
        ("a\n\nb", 2, "a\nX\nb"),
        ("", 0, "X"),
    ] {
        let (mut state, buffer, view) = editing(text, cursor);
        state.registers.set(
            '"',
            RegisterValue {
                text: "X".into(),
                shape: RegisterShape::Character,
            },
            true,
            true,
        );
        apply(&mut state, "edit.put", &[("before", "false")], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), cursor);
    }
}

#[test]
fn characterwise_put_lands_on_the_last_inserted_grapheme() {
    use crate::editor::RegisterShape;
    use crate::editor::RegisterValue;
    for before in ["true", "false"] {
        for (text, count, expected, cursor) in [
            ("XY", 1, "XY", 1),
            ("Xe\u{301}", 2, "Xe\u{301}Xe\u{301}", 5),
            ("X\r\nY", 1, "X\nY", 0),
        ] {
            let (mut state, buffer, view) = editing("ab", 0);
            state.registers.set(
                '"',
                RegisterValue {
                    text: text.into(),
                    shape: RegisterShape::Character,
                },
                true,
                true,
            );
            apply(&mut state, "edit.put", &[("before", before)], count);
            let at = usize::from(before == "false");
            assert_eq!(
                state.buffers[buffer].text.to_string(),
                if at == 0 {
                    format!("{expected}ab")
                } else {
                    format!("a{expected}b")
                }
            );
            assert_eq!(state.views[view].cursor.get(), at + cursor);
            if !expected.contains('\n') {
                apply(&mut state, "edit.delete_char", &[], 1);
                let remaining = &expected[..cursor];
                assert_eq!(
                    state.buffers[buffer].text.to_string(),
                    if at == 0 {
                        format!("{remaining}ab")
                    } else {
                        format!("a{remaining}b")
                    }
                );
            }
        }
    }
}

#[test]
fn block_put_extends_eof_and_repeats_rows_horizontally() {
    use crate::editor::RegisterShape;
    use crate::editor::RegisterValue;
    for (text, cursor, expected) in [
        ("x", 0, "aax\nbb\ncc"),
        ("xx", 1, "xaax\n bb\n cc"),
        ("x\nx\nx\nx", 0, "aax\nbbx\nccx\nx"),
    ] {
        let (mut state, buffer, _) = editing(text, cursor);
        state.registers.set(
            '"',
            RegisterValue {
                text: "a\nb\nc".into(),
                shape: RegisterShape::Block,
            },
            true,
            true,
        );
        apply(&mut state, "edit.put", &[("before", "true")], 2);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn counted_block_put_preserves_short_rows_and_display_width() {
    use crate::editor::RegisterShape;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    for (source, register, expected) in [
        ("ab\nx\ncd", "ab\nx\ncd", "abab|\nx x |\ncdcd|"),
        ("界\nx\nab", "界\nx\nab", "界界|\nx x |\nabab|"),
        (
            "\tq\nx\nabcd",
            "\t\nx\nabcd",
            "        |\nx   x   |\nabcdabcd|",
        ),
        (
            "e\u{301}b\nx\ncd",
            "e\u{301}b\nx\ncd",
            "e\u{301}be\u{301}b|\nx x |\ncdcd|",
        ),
    ] {
        let (mut state, _, view) = editing(source, source.len() - 1);
        apply(&mut state, "mode.visual_block", &[], 1);
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(source.len() - 1),
            kind: SelectionKind::Block,
        });
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "yank"),
                ("motion", "visual"),
                ("linewise", "false"),
            ],
            1,
        );
        let value = state.registers.get('"').unwrap();
        assert_eq!(value.shape, RegisterShape::Block);
        assert_eq!(value.text, register);
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("|\n|\n|"), None)
            .unwrap();
        apply(&mut state, "edit.put", &[("before", "true")], 2);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        apply(&mut state, "history.undo", &[], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "|\n|\n|");
    }
}

#[test]
fn counted_block_put_pads_empty_rows_and_expands_tabs_at_the_destination() {
    use crate::editor::RegisterShape;
    use crate::editor::RegisterValue;
    let (mut state, buffer, view) = editing("a|\nb|\nc|", 1);
    state.registers.set(
        '"',
        RegisterValue {
            text: "\t\n\nx".into(),
            shape: RegisterShape::Block,
        },
        true,
        true,
    );
    apply(&mut state, "edit.put", &[("before", "true")], 2);
    assert_eq!(
        state.buffers[buffer].text.to_string(),
        "a      |\nb      |\ncx  x  |"
    );
    assert_eq!(state.views[view].cursor.get(), 1);
    assert_eq!(state.registers.get('"').unwrap().text, "\t\n\nx");
    apply(&mut state, "history.undo", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "a|\nb|\nc|");
}

#[test]
fn counted_history_commands_stop_at_history_boundaries() {
    let (mut state, buffer, _) = editing("abc", 0);
    for text in ["X", "Y", "Z"] {
        apply(&mut state, "edit.replace", &[("text", text)], 1);
        apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    }
    assert_eq!(state.buffers[buffer].text.to_string(), "XYZ");
    for (command, count, expected) in [
        ("history.undo", 2, "Xbc"),
        ("history.redo", 2, "XYZ"),
        ("history.undo", 99, "abc"),
        ("history.redo", 99, "XYZ"),
    ] {
        apply(&mut state, command, &[], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
    apply(&mut state, "history.undo", &[], 2);
    apply(&mut state, "edit.replace", &[("text", "Q")], 1);
    let branched = state.buffers[buffer].text.to_string();
    apply(&mut state, "history.undo", &[], u64::MAX);
    assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    apply(&mut state, "history.redo", &[], u64::MAX);
    assert_eq!(state.buffers[buffer].text.to_string(), branched);
}

#[test]
fn counted_history_commands_preserve_empty_history_errors() {
    use crate::editor::transaction::TransactionError;
    let (mut state, buffer, _) = editing("abc", 0);
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    for id in ["history.undo", "history.redo"] {
        let mut invocation = CommandInvocation::new(id, InvocationSource::Test).unwrap();
        invocation.count = std::num::NonZeroU64::new(u64::MAX);
        let outcome = registry.execute(&state, &invocation).unwrap();
        let error = state
            .apply_transaction(&outcome.transaction.unwrap())
            .unwrap_err();
        assert!(matches!(
            (id, error),
            ("history.undo", TransactionError::NothingToUndo)
                | ("history.redo", TransactionError::NothingToRedo)
        ));
        assert_eq!(state.buffers[buffer].text.to_string(), "abc");
    }
}

#[test]
fn counted_insert_repeats_the_finished_text_and_undoes_together() {
    for placement in [
        "before",
        "after",
        "line_start",
        "line_end",
        "open_above",
        "open_below",
    ] {
        let (mut state, buffer, view) = editing("z", 0);
        state.policy.autopairs = false;
        apply(&mut state, "mode.insert", &[("placement", placement)], 3);
        apply(&mut state, "edit.insert_text", &[("text", "e\u{301}x")], 1);
        apply(&mut state, "edit.backspace", &[], 1);
        apply(&mut state, "mode.normal", &[], 1);
        let expected = match placement {
            "before" | "line_start" => "e\u{301}e\u{301}e\u{301}z",
            "after" | "line_end" => "ze\u{301}e\u{301}e\u{301}",
            "open_above" => "e\u{301}\ne\u{301}\ne\u{301}\nz",
            _ => "z\ne\u{301}\ne\u{301}\ne\u{301}",
        };
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            expected,
            "{placement}"
        );
        assert_eq!(state.views[view].mode, crate::editor::EditorMode::Normal);
        apply(&mut state, "history.undo", &[], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "z", "{placement}");
    }
}

#[test]
fn counted_replace_requires_the_whole_count_and_lands_on_the_last_grapheme() {
    for text in ["ab", "ab\ncd"] {
        let (mut state, buffer, view) = editing(text, 0);
        apply(&mut state, "edit.replace", &[("text", "x")], 3);
        assert_eq!(state.buffers[buffer].text.to_string(), text);
        assert_eq!(state.views[view].cursor.get(), 0);
    }
    let (mut state, buffer, view) = editing("abc", 0);
    apply(&mut state, "edit.replace", &[("text", "e\u{301}")], 2);
    assert_eq!(state.buffers[buffer].text.to_string(), "e\u{301}e\u{301}c");
    assert_eq!(state.views[view].cursor.get(), 3);
    apply(&mut state, "edit.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "e\u{301}c");
}

#[test]
fn counted_replace_uses_normalized_text_and_the_final_grapheme() {
    for (replacement, count, expected, cursor) in [("\r\n", 3, "\n\n\n", 2), ("éx", 2, "éxéxc", 5)]
    {
        let (mut state, buffer, view) = editing("abc", 0);
        apply(&mut state, "edit.replace", &[("text", replacement)], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), cursor);
    }
}

#[test]
fn normal_deletions_stay_on_the_last_remaining_character() {
    for text in ["abe\u{301}", "abe\u{301}\nnext"] {
        for operator in [false, true] {
            let (mut state, buffer, view) = editing(text, 2);
            if operator {
                apply(
                    &mut state,
                    "edit.operator",
                    &[
                        ("operator", "delete"),
                        ("motion", "line_end"),
                        ("linewise", "false"),
                    ],
                    1,
                );
            } else {
                apply(&mut state, "edit.delete_char", &[], 1);
            }
            assert_eq!(state.views[view].cursor.get(), 1);
            apply(&mut state, "edit.delete_char", &[], 1);
            assert_eq!(state.views[view].cursor.get(), 0);
            assert_eq!(
                state.buffers[buffer].text.to_string(),
                if text.contains('\n') { "a\nnext" } else { "a" }
            );
        }
    }
    let (mut state, buffer, view) = editing("abc", 2);
    state.views[view].mode = crate::editor::EditorMode::Insert;
    apply(&mut state, "edit.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "ab");
    assert_eq!(state.views[view].cursor.get(), 2);
}

#[test]
fn switching_visual_modes_preserves_endpoints_and_updates_the_operator_shape() {
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    let (mut state, buffer, view) = editing("abc\ndef\n", 1);
    apply(&mut state, "mode.visual_char", &[], 1);
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    for (command, kind) in [
        ("mode.visual_line", SelectionKind::Line),
        ("mode.visual_block", SelectionKind::Block),
        ("mode.visual_char", SelectionKind::Character),
        ("mode.visual_line", SelectionKind::Line),
    ] {
        apply(&mut state, command, &[], 1);
        let selection = state.views[view].selection.unwrap();
        assert_eq!(selection.anchor, ByteOffset(1));
        assert_eq!(selection.head, ByteOffset(2));
        assert_eq!(selection.kind, kind);
    }
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "delete"),
            ("motion", "visual"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "def\n");
}

#[test]
fn visual_put_repeats_the_register_before_replacing_the_selection() {
    let (mut state, buffer, view) = editing("abc", 1);
    state.registers.set(
        '"',
        crate::editor::RegisterValue {
            text: "x".into(),
            shape: crate::editor::RegisterShape::Character,
        },
        true,
        true,
    );
    apply(&mut state, "mode.visual_char", &[], 1);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "replace"),
            ("motion", "visual"),
            ("linewise", "false"),
        ],
        2,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "axxc");
    assert_eq!(state.registers.get('"').unwrap().text, "b");
    assert_eq!(state.views[view].mode, crate::editor::EditorMode::Normal);
}

#[test]
fn visual_linewise_put_terminates_an_unterminated_yanked_line() {
    let (mut state, buffer, view) = editing("one\ntwo", 4);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "yank"),
            ("motion", "line"),
            ("linewise", "true"),
        ],
        1,
    );
    assert_eq!(state.registers.get('"').unwrap().text, "two");
    state.views[view].cursor = crate::editor::text::ByteOffset(0);
    apply(&mut state, "mode.visual_line", &[], 1);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "replace"),
            ("motion", "visual"),
            ("linewise", "true"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "two\ntwo");
}

#[test]
fn normal_linewise_yank_keeps_the_cursor_column() {
    let (mut state, _, view) = editing("abcdef", 2);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "yank"),
            ("motion", "line"),
            ("linewise", "true"),
        ],
        1,
    );
    assert_eq!(state.views[view].cursor.get(), 2);
    assert_eq!(state.registers.get('"').unwrap().text, "abcdef");
}

#[test]
fn visual_yank_returns_to_the_start_of_the_selection() {
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    for (mode, kind, anchor, head, expected_cursor, expected_text) in [
        (
            EditorMode::VisualChar,
            SelectionKind::Character,
            0,
            2,
            0,
            "abc",
        ),
        (
            EditorMode::VisualChar,
            SelectionKind::Character,
            2,
            0,
            0,
            "abc",
        ),
        (
            EditorMode::VisualLine,
            SelectionKind::Line,
            2,
            8,
            0,
            "abcdef\nghijkl",
        ),
        (
            EditorMode::VisualBlock,
            SelectionKind::Block,
            9,
            1,
            1,
            "bc\nhi",
        ),
    ] {
        let (mut state, buffer, view) = editing("abcdef\nghijkl", head);
        state.views[view].mode = mode;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(anchor),
            head: ByteOffset(head),
            kind,
        });
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "yank"),
                ("motion", "visual"),
                ("linewise", "false"),
            ],
            1,
        );
        assert_eq!(state.views[view].cursor.get(), expected_cursor);
        assert_eq!(state.views[view].mode, EditorMode::Normal);
        assert!(state.views[view].selection.is_none());
        assert_eq!(state.registers.get('"').unwrap().text, expected_text);
        assert_eq!(state.buffers[buffer].text.to_string(), "abcdef\nghijkl");
        assert!(state.buffers[buffer].undo.is_empty());
    }
}

#[test]
fn crlf_insert_and_replace_use_normalized_cursor_lengths() {
    for command in ["edit.insert_text", "edit.replace_text"] {
        let (mut state, buffer, view) = editing("", 0);
        state.views[view].mode = crate::editor::EditorMode::Insert;
        apply(&mut state, command, &[("text", "a\r\nb")], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "a\nb");
        assert_eq!(state.views[view].cursor.get(), 3);
    }
}

#[test]
fn normal_horizontal_motion_stays_off_line_endings() {
    let (mut state, _, view) = editing("ab\ncd\n", 1);
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    assert_eq!(state.views[view].cursor.get(), 1);
    state.views[view].mode = crate::editor::EditorMode::Insert;
    apply(&mut state, "view.cursor.move", &[("motion", "right")], 1);
    assert_eq!(state.views[view].cursor.get(), 2);
}

#[test]
fn replace_backspace_never_restores_text_over_another_views_edit() {
    let (mut state, buffer, first) = editing("abc", 0);
    let second = state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    state.focus_pane(-1, true).unwrap();
    assert_eq!(state.active_view(), Some(first));
    apply(&mut state, "mode.replace", &[], 1);
    apply(&mut state, "edit.replace_text", &[("text", "X")], 1);
    state.focus_pane(1, true).unwrap();
    assert_eq!(state.active_view(), Some(second));
    state.views[second].cursor = crate::editor::text::ByteOffset(0);
    apply(&mut state, "mode.replace", &[], 1);
    apply(&mut state, "edit.replace_text", &[("text", "Y")], 1);
    state.focus_pane(-1, true).unwrap();
    assert!(state.views[first].replace_history.is_empty());
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "Ybc");
}

#[test]
fn replace_backspace_preserves_earlier_records_and_remaps_unaffected_ones() {
    let (mut state, buffer, view) = editing("abc", 0);
    apply(&mut state, "mode.replace", &[], 1);
    apply(&mut state, "edit.replace_text", &[("text", "X")], 1);
    apply(&mut state, "edit.replace_text", &[("text", "Y")], 1);
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.views[view].replace_history.len(), 1);
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "abc");

    state.views[view].cursor = crate::editor::text::ByteOffset(1);
    apply(&mut state, "edit.replace_text", &[("text", "X")], 1);
    let second = state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    state.views[second].cursor = crate::editor::text::ByteOffset(0);
    state.views[second].mode = crate::editor::EditorMode::Insert;
    apply(&mut state, "edit.insert_text", &[("text", "zz")], 1);
    state.focus_pane(-1, true).unwrap();
    assert_eq!(state.views[view].replace_history[0].start.get(), 3);
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "zzabc");
}

#[test]
fn counted_horizontal_and_word_motions_stop_at_boundaries() {
    let source = format!("{}\nlast", "e\u{301}".repeat(20_000));
    let (mut state, _, view) = editing(&source, 0);
    for motion in ["left", "word_backward", "big_word_backward"] {
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", motion)],
            1_000_000,
        );
        assert_eq!(state.views[view].cursor.get(), 0, "{motion}");
    }
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "right")],
        1_000_000,
    );
    assert_eq!(state.views[view].cursor.get(), source.len() - 8);
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "left")],
        1_000_000,
    );
    assert_eq!(state.views[view].cursor.get(), 0);
    for motion in [
        "word_forward",
        "big_word_forward",
        "word_end",
        "big_word_end",
    ] {
        state.views[view].cursor = crate::editor::text::ByteOffset(0);
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", motion)],
            1_000_000,
        );
        assert_eq!(state.views[view].cursor.get(), source.len() - 1, "{motion}");
    }
}

#[test]
fn counted_line_end_moves_and_deletes_through_the_requested_line() {
    for (count, cursor, remaining) in [(2, 7, "a\nlast\n"), (100, 12, "a\n")] {
        let (mut state, buffer, view) = editing("abc\ndefg\nlast\n", 1);
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "line_end")],
            count,
        );
        assert_eq!(state.views[view].cursor.get(), cursor);
        state.views[view].cursor = crate::editor::text::ByteOffset(1);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", "line_end"),
                ("linewise", "false"),
            ],
            count,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), remaining);
    }
    for (source, cursor, count, expected) in [
        ("a\n\nb", 2, 1, "a\n\nb"),
        ("a\nb", 0, 1, "\nb"),
        ("a", 0, 1, ""),
        ("a\nb\nc", 0, 2, "\nc"),
        ("a\n\nb", 0, 2, "\nb"),
        ("e\u{301}\nb", 0, 1, "\nb"),
    ] {
        let (mut state, buffer, view) = editing(source, cursor);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", "line_end"),
                ("linewise", "false"),
            ],
            count,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        if source == "a\n\nb" && cursor == 2 {
            assert_eq!(state.views[view].cursor.get(), 2);
        }
    }
}

#[test]
fn counted_percent_navigates_lines_and_document_start_skips_indent() {
    let source = "  line\n".repeat(100);
    let (mut state, _, view) = editing(&source, 70);
    for (count, line) in [(1, 0), (50, 49), (100, 99)] {
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "matching_pair")],
            count,
        );
        assert_eq!(state.views[view].cursor.get(), line * 7 + 2);
    }
    for count in [0, 1] {
        state.views[view].cursor = crate::editor::text::ByteOffset(70);
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "document_start")],
            count,
        );
        assert_eq!(state.views[view].cursor.get(), 2);
    }
}

#[test]
fn matching_pairs_ignore_quoted_and_escaped_delimiters_in_both_directions() {
    for source in [
        r#"( ")" )"#,
        r#"( '(' ')' )"#,
        r"( `)` )",
        r#"( "escaped \" )" )"#,
        r"( \) )",
        r"( \\() )",
        "(\n  \"quoted )\"\n)",
        r#""ignored (" ( ")" )"#,
        "(don't)",
        "('unterminated)",
        "(&'a str, &'b str)",
        "(don't 'x')",
    ] {
        let opening = if source.starts_with('"') {
            source.find(" ( ").unwrap() + 1
        } else {
            0
        };
        let closing = source.len() - 1;
        let (mut state, _, view) = editing(source, opening);
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "matching_pair")],
            0,
        );
        assert_eq!(state.views[view].cursor.get(), closing, "{source}");
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "matching_pair")],
            0,
        );
        assert_eq!(state.views[view].cursor.get(), opening, "{source}");
    }
}

#[test]
fn matching_pairs_handle_unbalanced_prefixes_and_rust_lifetimes() {
    for source in [")(()", "fn f<'a, 'b>(x: &'a str)"] {
        let opening = source.rfind('(').unwrap();
        let closing = source.len() - 1;
        let (mut state, _, view) = editing(source, opening);
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "matching_pair")],
            0,
        );
        assert_eq!(state.views[view].cursor.get(), closing, "{source}");
        apply(
            &mut state,
            "view.cursor.move",
            &[("motion", "matching_pair")],
            0,
        );
        assert_eq!(state.views[view].cursor.get(), opening, "{source}");
    }
}

#[test]
fn counted_percent_operators_cover_whole_lines() {
    for cursor in [0, 8] {
        let (mut state, buffer, _) = editing("  one\ntwo\nthree\nfour\n", cursor);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", "matching_pair"),
                ("linewise", "false"),
            ],
            50,
        );
        let expected = if cursor == 0 {
            "three\nfour\n"
        } else {
            "  one\nthree\nfour\n"
        };
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(
            state.registers.get('"').unwrap().shape,
            crate::editor::RegisterShape::Line
        );
    }
}

#[test]
fn replace_backspace_does_not_restore_across_cursor_movement() {
    let (mut state, buffer, view) = editing("abc\ndef", 0);
    apply(&mut state, "mode.replace", &[], 1);
    apply(&mut state, "edit.replace_text", &[("text", "X")], 1);
    apply(&mut state, "view.cursor.move", &[("motion", "down")], 1);
    let cursor = state.views[view].cursor;
    apply(&mut state, "edit.replace_backspace", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "Xbc\ndef");
    assert_eq!(state.views[view].cursor, cursor);
    assert!(state.views[view].replace_history.is_empty());
}

#[test]
fn backward_inclusive_operators_include_the_cursor_character() {
    for (text, cursor, motion, character, expected) in [
        ("(abc)", 4, "matching_pair", "(", ""),
        ("abc", 2, "find_backward", "a", ""),
        ("abc", 2, "till_backward", "a", "a"),
        ("abcb", 3, "till_backward_again", "a", "a"),
    ] {
        let (mut state, buffer, _) = editing(text, cursor);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", motion),
                ("character", character),
                ("linewise", "false"),
            ],
            0,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn word_motions_and_objects_keep_combining_graphemes_whole() {
    for (motion, cursor, expected) in [
        ("word_forward", 0, 5),
        ("word_backward", 5, 0),
        ("word_end", 0, 3),
    ] {
        let (mut state, _, view) = editing("e\u{301}x y", cursor);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), expected, "{motion}");
    }
    let (mut state, buffer, _) = editing("e\u{301} test", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "delete"),
            ("motion", "inner_word"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), " test");
    let (mut state, buffer, _) = editing("e\u{301}!", 1);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "delete"),
            ("motion", "inner_word"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "!");
}

#[test]
fn repeated_till_skips_an_adjacent_target_after_a_combining_grapheme() {
    let (mut state, _, view) = editing("e\u{301}x ax", 0);
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "till_forward"), ("character", "x")],
        1,
    );
    assert_eq!(state.views[view].cursor.get(), 0);
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "till_forward_again"), ("character", "x")],
        1,
    );
    assert_eq!(state.views[view].cursor.get(), 5);
    let (mut state, _, view) = editing("xax e\u{301}z", 4);
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "till_backward"), ("character", "x")],
        1,
    );
    assert_eq!(state.views[view].cursor.get(), 3);
    apply(
        &mut state,
        "view.cursor.move",
        &[("motion", "till_backward_again"), ("character", "x")],
        1,
    );
    assert_eq!(state.views[view].cursor.get(), 1);
}

#[test]
fn word_motions_stop_on_empty_lines_in_both_directions() {
    for (motion, cursor, count, expected) in [
        ("word_forward", 0, 1, 4),
        ("word_forward", 0, 2, 5),
        ("big_word_forward", 0, 1, 4),
        ("big_word_forward", 4, 1, 5),
        ("word_backward", 5, 1, 4),
        ("word_backward", 5, 2, 0),
        ("big_word_backward", 5, 1, 4),
        ("big_word_backward", 4, 1, 0),
    ] {
        let (mut state, _, view) = editing("one\n\ntwo", cursor);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], count);
        assert_eq!(state.views[view].cursor.get(), expected, "{motion}");
    }
}

#[test]
fn word_forward_operators_keep_the_final_words_newline() {
    for motion in ["word_forward", "big_word_forward"] {
        for (source, cursor, count, expected) in [
            ("one\ntwo", 0, 1, "\ntwo"),
            ("\ntwo", 0, 1, "two"),
            ("one  \n  two", 0, 1, "\n  two"),
            ("one two\nthree", 0, 2, "\nthree"),
            ("one\ntwo three", 0, 2, "three"),
            ("one\n  \ntwo", 0, 1, "\n  \ntwo"),
            ("one\ntwo\nthree", 0, 2, "three"),
            ("one\ntwo\nthree", 1, 2, "o\nthree"),
            ("one\n\ntwo", 0, 2, "two"),
            ("one\n\ntwo", 1, 2, "o\ntwo"),
            ("  one\ntwo\nthree", 2, 2, "three"),
            ("one\ntwo\n", 0, 100, ""),
        ] {
            let (mut state, buffer, _) = editing(source, cursor);
            apply(
                &mut state,
                "edit.operator",
                &[
                    ("operator", "delete"),
                    ("motion", motion),
                    ("linewise", "false"),
                ],
                count,
            );
            assert_eq!(state.buffers[buffer].text.to_string(), expected, "{motion}");
        }
    }
}

#[test]
fn repeated_word_end_advances_but_change_word_keeps_its_space() {
    for motion in ["word_end", "big_word_end"] {
        let (mut state, _, view) = editing("one two", 0);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), 2);
        apply(&mut state, "view.cursor.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].cursor.get(), 6);
    }
    let (mut state, buffer, _) = editing("a two", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "change"),
            ("motion", "word_forward"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), " two");
}

#[test]
fn counted_word_and_paragraph_objects_extend_the_range() {
    for (text, motion, expected) in [
        ("one two three", "a_word", "three"),
        ("one\n\ntwo\n\nthree", "a_paragraph", "three"),
    ] {
        let (mut state, buffer, _) = editing(text, 0);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", motion),
                ("linewise", "false"),
            ],
            2,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn counted_inner_words_include_whitespace_runs() {
    for (text, motion, count, expected) in [
        ("one two three", "inner_word", 2, "two three"),
        ("one two three", "inner_word", 3, " three"),
        ("one \t two three", "inner_big_word", 2, "two three"),
        ("one! two", "inner_word", 2, " two"),
        ("one! two", "inner_big_word", 2, "two"),
        ("e\u{301} e\u{301}!", "inner_word", 2, "e\u{301}!"),
    ] {
        let (mut state, buffer, _) = editing(text, 0);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", motion),
                ("linewise", "false"),
            ],
            count,
        );
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            expected,
            "{motion}: {text}"
        );
    }
}

#[test]
fn counted_text_objects_materialize_the_document_once() {
    use crate::editor::text::ByteOffset;
    for (unit, motion) in [
        ("word ", "a_word"),
        ("word! ", "a_big_word"),
        ("word\n\n", "a_paragraph"),
    ] {
        let text = TextBuffer::from_text(&unit.repeat(10_000));
        let before = TextBuffer::test_materializations().1;
        let range = crate::editor::command::builtins::text_object::text_object(
            &text,
            &[],
            ByteOffset(0),
            motion,
            10_000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            TextBuffer::test_materializations().1 - before,
            1,
            "{motion}"
        );
        assert_eq!(range.end.get(), text.len_bytes());
    }
}

#[test]
fn syntax_objects_do_not_fall_forward_when_an_enclosing_count_is_missing() {
    use crate::editor::SyntaxObject;
    use crate::editor::SyntaxObjectKind;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    let text = TextBuffer::from_text(" (a) (b)");
    for (kind, motion) in [
        (SyntaxObjectKind::Function, "inner_function"),
        (SyntaxObjectKind::Class, "inner_class"),
        (SyntaxObjectKind::Argument, "inner_argument"),
    ] {
        let objects = [
            SyntaxObject {
                kind,
                outer: (1, 4),
                inner: (2, 3),
            },
            SyntaxObject {
                kind,
                outer: (5, 8),
                inner: (6, 7),
            },
        ];
        assert!(
            crate::editor::command::builtins::text_object::text_object(
                &text,
                &objects,
                ByteOffset(2),
                motion,
                2
            )
            .is_err()
        );
        for cursor in [0, 2] {
            assert_eq!(
                crate::editor::command::builtins::text_object::text_object(
                    &text,
                    &objects,
                    ByteOffset(cursor),
                    motion,
                    1
                )
                .unwrap(),
                Some(ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap()),
            );
        }
        let nested = [
            objects[0],
            SyntaxObject {
                kind,
                outer: (0, 8),
                inner: (1, 8),
            },
        ];
        assert_eq!(
            crate::editor::command::builtins::text_object::text_object(
                &text,
                &nested,
                ByteOffset(2),
                motion,
                2
            )
            .unwrap(),
            Some(ByteRange::new(ByteOffset(1), ByteOffset(8)).unwrap()),
        );
    }
}

#[test]
fn bracket_objects_do_not_fall_forward_when_an_enclosing_count_is_missing() {
    use crate::editor::text::ByteOffset;
    for (text, cursor, motion) in [
        ("(a) (b)", 1, "inner_paren"),
        ("(a) (b)", 0, "inner_paren"),
        ("(a) (b)", 2, "inner_paren"),
        ("[a] [b]", 1, "inner_bracket"),
        ("{a} {b}", 1, "inner_brace"),
        ("<a> <b>", 1, "inner_angle"),
    ] {
        let (state, buffer, _) = editing(text, cursor);
        assert!(
            crate::editor::command::builtins::text_object::text_object(
                &state.buffers[buffer].text,
                &[],
                ByteOffset(cursor),
                motion,
                2
            )
            .is_err()
        );
    }
    for (text, cursor, count, expected) in
        [(" (a) (b)", 0, 1, " () (b)"), ("((a)) (b)", 2, 2, "() (b)")]
    {
        let (mut state, buffer, _) = editing(text, cursor);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "delete"),
                ("motion", "inner_paren"),
                ("linewise", "false"),
            ],
            count,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn multi_word_matches_keep_combining_graphemes_whole() {
    for command in ["multi.add_next", "multi.add_all"] {
        let (mut state, buffer, view) = editing("e\u{301} e\u{301} xe\u{301} e\u{301}x", 1);
        apply(&mut state, command, &[], 1);
        if command == "multi.add_next" {
            apply(&mut state, command, &[], 1);
        }
        assert_eq!(state.views[view].multi_marks.len(), 2);
        for mark in &state.views[view].multi_marks {
            assert_eq!(state.buffers[buffer].text.slice(*mark).unwrap(), "e\u{301}");
        }
        apply(&mut state, "multi.delete", &[], 1);
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            "  xe\u{301} e\u{301}x"
        );
    }
}

#[test]
fn multi_visual_matches_remain_literal_after_marking() {
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    for command in ["multi.add_next", "multi.add_all", "multi.skip"] {
        let (mut state, _, view) = editing("foobar xfoo yfoo", 0);
        state.views[view].mode = EditorMode::VisualChar;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(2),
            kind: SelectionKind::Character,
        });
        apply(&mut state, "multi.add_next", &[], 1);
        apply(&mut state, command, &[], 1);
        assert!(
            state.views[view]
                .multi_marks
                .iter()
                .any(|mark| mark.start == ByteOffset(8))
        );
        apply(&mut state, "multi.add_all", &[], 1);
        assert_eq!(state.views[view].multi_marks.len(), 3);
    }
    let (mut state, _, view) = editing("foo foobar xfoo foo", 0);
    apply(&mut state, "multi.add_next", &[], 1);
    apply(&mut state, "multi.add_all", &[], 1);
    assert_eq!(state.views[view].multi_marks.len(), 2);
}

#[test]
fn multi_next_after_mouse_movement_uses_a_mark_boundary() {
    let (mut state, _, view) = editing("a é a", 0);
    apply(&mut state, "multi.add_next", &[], 1);
    state.views[view].cursor = crate::editor::text::ByteOffset(2);
    apply(&mut state, "multi.add_next", &[], 1);
    assert_eq!(state.views[view].cursor.get(), 5);
    assert_eq!(state.views[view].multi_marks.len(), 2);
}

#[test]
fn multi_select_all_rejects_empty_visual_selection() {
    let (mut state, _, view) = editing("abc", 3);
    apply(&mut state, "mode.visual_char", &[], 1);
    let mut registry = CommandRegistry::<()>::default();
    register_builtin_commands(&mut registry).unwrap();
    let invocation = CommandInvocation::new("multi.add_all", InvocationSource::Test).unwrap();
    assert!(registry.execute(&state, &invocation).is_err());
    assert!(state.views[view].multi_marks.is_empty());
}

#[test]
fn multi_line_additions_keep_the_requested_column_across_short_lines() {
    for (cursor, command, expected) in [(4, "multi.add_below", 13), (13, "multi.add_above", 4)] {
        let (mut state, _, view) = editing("abcdef\nx\nabcdef", cursor);
        apply(&mut state, command, &[], 1);
        apply(&mut state, command, &[], 1);
        assert_eq!(state.views[view].cursor.get(), expected);
        assert_eq!(state.views[view].multi_marks.len(), 3);
    }
}

#[test]
fn multi_vertical_motion_keeps_each_cursors_wanted_column() {
    use crate::editor::EditorMode;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    let (mut state, _, view) = editing("abcdef\nx\nabcdef\nabcdef\nx\nabcdef", 4);
    state.views[view].mode = EditorMode::Multi;
    state.views[view].multi_marks = vec![
        ByteRange::empty(ByteOffset(4)),
        ByteRange::empty(ByteOffset(18)),
    ];
    for (motion, expected) in [
        ("down", vec![7, 23]),
        ("down", vec![13, 27]),
        ("up", vec![7, 23]),
        ("up", vec![4, 18]),
    ] {
        apply(&mut state, "multi.move", &[("motion", motion)], 1);
        assert_eq!(
            state.views[view]
                .multi_marks
                .iter()
                .map(|mark| mark.start.get())
                .collect::<Vec<_>>(),
            expected
        );
    }
    apply(&mut state, "multi.move", &[("motion", "down")], 1);
    apply(&mut state, "multi.move", &[("motion", "line_start")], 1);
    apply(&mut state, "multi.move", &[("motion", "down")], 1);
    assert_eq!(
        state.views[view]
            .multi_marks
            .iter()
            .map(|mark| mark.start.get())
            .collect::<Vec<_>>(),
        vec![9, 25]
    );
}

#[test]
fn adjacent_multi_character_deletions_deduplicate_collapsed_cursors() {
    use crate::editor::EditorMode;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    let (mut state, buffer, view) = editing("::x", 0);
    state.views[view].mode = EditorMode::Multi;
    state.views[view].multi_marks = vec![
        ByteRange::empty(ByteOffset(0)),
        ByteRange::empty(ByteOffset(1)),
    ];
    apply(&mut state, "multi.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "x");
    assert_eq!(
        state.views[view].multi_marks,
        vec![ByteRange::empty(ByteOffset(0))]
    );
    apply(&mut state, "multi.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "");
}

#[test]
fn multi_block_append_pads_short_rows_to_the_rectangle_edge() {
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    for (source, expected) in [
        ("abc\nx\nabc", "abc!\nx  !\nabc!"),
        ("x\nabc\nabc", "x  !\nabc!\nabc!"),
        ("abc\n\nabc", "abc!\n   !\nabc!"),
    ] {
        let (mut state, buffer, view) = editing(source, 0);
        state.views[view].mode = EditorMode::VisualBlock;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(source.len() - 1),
            kind: SelectionKind::Block,
        });
        apply(&mut state, "multi.insert", &[("placement", "end")], 1);
        assert_eq!(state.views[view].cursor, state.views[view].block_insert[0]);
        apply(&mut state, "edit.insert_text", &[("text", "!")], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn multi_block_change_deletes_the_supplied_visual_ranges() {
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    let (mut state, buffer, view) = editing("abc\nx\nabc", 0);
    state.views[view].mode = EditorMode::VisualBlock;
    state.views[view].selection = Some(Selection {
        anchor: ByteOffset(0),
        head: ByteOffset(7),
        kind: SelectionKind::Block,
    });
    apply(&mut state, "multi.insert", &[("placement", "change")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "c\n\nc");
    apply(&mut state, "edit.insert_text", &[("text", "!")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "!c\n!\n!c");
}

#[test]
fn adjacent_multi_changes_keep_one_insertion_at_each_resulting_position() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    let (mut state, buffer, view) = editing("::x", 0);
    state.views[view].mode = crate::editor::EditorMode::Multi;
    state.views[view].multi_marks = vec![
        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
        ByteRange::new(ByteOffset(1), ByteOffset(2)).unwrap(),
    ];
    apply(&mut state, "multi.insert", &[("placement", "change")], 1);
    apply(&mut state, "edit.insert_text", &[("text", "a")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "ax");
}

#[test]
fn multi_append_advances_empty_marks_and_line_cursors_use_display_columns() {
    let (mut state, buffer, _) = editing("ab\nab", 0);
    apply(&mut state, "multi.add_below", &[], 1);
    apply(&mut state, "multi.insert", &[("placement", "end")], 1);
    apply(&mut state, "edit.insert_text", &[("text", "X")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "aXb\naXb");
    let (mut state, _, view) = editing("\tx\n123456789", 1);
    state.policy.tab_width = 4;
    apply(&mut state, "multi.add_below", &[], 1);
    assert_eq!(state.views[view].cursor.get(), 7);
}

#[test]
fn escaped_quotes_insert_before_the_autopaired_closer() {
    let (mut state, buffer, _) = editing("", 0);
    state.policy.autopairs = true;
    for text in ["\"", "\\", "\""] {
        apply(&mut state, "edit.insert_text", &[("text", text)], 1);
    }
    assert_eq!(state.buffers[buffer].text.to_string(), "\"\\\"\"");
}

#[test]
fn empty_objects_can_be_surrounded_and_symmetric_surrounds_removed() {
    let (mut state, buffer, _) = editing("()", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "surround_add"),
            ("motion", "inner_paren"),
            ("linewise", "false"),
            ("character", "]"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "([])");
    let (mut state, buffer, _) = editing("*word*", 1);
    apply(
        &mut state,
        "edit.surround",
        &[("character", "*"), ("action", "delete")],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "word");
}

#[test]
fn backslash_surrounds_can_be_deleted_and_replaced() {
    for (action, expected) in [("delete", "word"), ("replace", "[word]")] {
        let (mut state, buffer, view) = editing("word", 0);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "surround_add"),
                ("motion", "inner_word"),
                ("linewise", "false"),
                ("character", "\\"),
            ],
            1,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), "\\word\\");
        state.views[view].cursor = crate::editor::text::ByteOffset(1);
        apply(
            &mut state,
            "edit.surround",
            &[
                ("action", action),
                ("character", "\\"),
                ("replacement", "]"),
            ],
            1,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
    let (mut state, buffer, _) = editing("\"a\\\"b\"", 1);
    apply(
        &mut state,
        "edit.surround",
        &[("action", "delete"), ("character", "\"")],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "a\\\"b");
}

#[test]
fn indentation_and_joins_accept_multibyte_endpoints() {
    let (mut state, buffer, _) = editing("é", 0);
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "indent"),
            ("motion", "line"),
            ("linewise", "true"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "    é");
    let (mut state, buffer, view) = editing("a   \né\n", 3);
    apply(&mut state, "edit.join_lines", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "a é\n");
    assert_eq!(state.views[view].cursor.get(), 1);
    let (mut state, buffer, view) = editing("a\né\n", 0);
    state.views[view].mode = crate::editor::EditorMode::VisualChar;
    state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(0),
        head: crate::editor::text::ByteOffset(2),
        kind: crate::editor::SelectionKind::Character,
    });
    apply(
        &mut state,
        "edit.operator",
        &[
            ("operator", "join"),
            ("motion", "visual"),
            ("linewise", "false"),
        ],
        1,
    );
    assert_eq!(state.buffers[buffer].text.to_string(), "a é\n");
}

#[test]
fn joins_preserve_the_final_newline_without_joining_a_synthetic_line() {
    for (source, cursor, count, expected, expected_cursor) in [
        ("a\n", 0, 1, "a\n", 0),
        ("a\nb\n", 2, 1, "a\nb\n", 2),
        ("a\nb\n", 0, 99, "a b\n", 1),
        ("a\nb", 0, 99, "a b", 1),
        ("", 0, 1, "", 0),
    ] {
        let (mut state, buffer, view) = editing(source, cursor);
        apply(&mut state, "edit.join_lines", &[], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), expected_cursor);
    }
}

#[test]
fn joins_keep_trailing_whitespace_on_only_the_final_joined_line() {
    for (source, count, expected) in [
        ("a\nb  \nc", 1, "a b  \nc"),
        ("a  \n  b \t\n c  \n", 3, "a b c  \n"),
        ("a\n \t\nb", 1, "a\nb"),
    ] {
        let (mut state, buffer, _) = editing(source, 0);
        apply(&mut state, "edit.join_lines", &[], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn joining_an_empty_line_keeps_the_cursor_on_the_last_grapheme() {
    for source in ["a\n\nb", "é\n\nb", "e\u{301}\n\nb"] {
        let (mut state, buffer, view) = editing(source, 0);
        apply(&mut state, "edit.join_lines", &[], 1);
        assert_eq!(state.views[view].cursor.get(), 0);
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            source.replacen("\n\n", "\n", 1)
        );
        apply(&mut state, "edit.delete_char", &[], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "\nb");
    }
}

#[test]
fn toggle_case_advances_over_replacement_bytes_and_stays_on_the_line() {
    let (mut state, buffer, view) = editing("ab", 0);
    apply(&mut state, "edit.toggle_case", &[], 1);
    assert_eq!(state.views[view].cursor.get(), 1);
    apply(&mut state, "edit.toggle_case", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "AB");
    assert_eq!(state.views[view].cursor.get(), 1);

    for (source, count, expected, expected_cursor) in [
        ("éx", 1, "Éx", 2),
        ("ßx", 1, "SSx", 2),
        ("ß\nx", 1, "SS\nx", 1),
        ("e\u{301}x\ny", 99, "E\u{301}X\ny", 3),
        ("e\u{301}\nx", 99, "E\u{301}\nx", 0),
        ("\nx", 99, "\nx", 0),
        ("", 1, "", 0),
    ] {
        let (mut state, buffer, view) = editing(source, 0);
        apply(&mut state, "edit.toggle_case", &[], count);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), expected_cursor);
    }
}

#[test]
fn counted_toggle_case_traverses_a_long_line_in_a_large_document() {
    let line = "e\u{301}".repeat(8192);
    let rest = "unchanged\n".repeat(8192);
    let (mut state, buffer, view) = editing(&format!("{line}\n{rest}"), 0);
    apply(&mut state, "edit.toggle_case", &[], 8192);
    assert_eq!(
        state.buffers[buffer].text.to_string(),
        format!("{}\n{rest}", "E\u{301}".repeat(8192))
    );
    assert_eq!(state.views[view].cursor.get(), 8191 * 3);
}

#[test]
fn indentation_shifts_display_columns_with_independent_tab_and_indent_widths() {
    for (source, indent, expand_tabs, tab_width, indent_width, expected) in [
        ("foo", true, false, 8, 4, "    foo"),
        ("    foo", true, false, 8, 4, "\tfoo"),
        ("\tfoo", false, false, 8, 4, "    foo"),
        ("  \tfoo", false, false, 8, 4, "    foo"),
        ("\t  foo", false, false, 8, 4, "      foo"),
        ("  foo", false, false, 8, 4, "foo"),
        ("\tfoo", true, true, 8, 4, "            foo"),
        ("\tfoo", false, true, 8, 4, "    foo"),
        ("foo", true, false, 2, 4, "\t\tfoo"),
        ("\n", true, false, 8, 4, "\n"),
    ] {
        let (mut state, buffer, view) = editing(source, 0);
        state.policy.tab_width = tab_width;
        state.policy.indent_width = indent_width;
        state.policy.expand_tabs = expand_tabs;
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", if indent { "indent" } else { "unindent" }),
                ("motion", "line"),
                ("linewise", "true"),
            ],
            1,
        );
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            expected,
            "{source:?}"
        );
        let expected_cursor = expected.find('f').unwrap_or(0);
        assert_eq!(state.views[view].cursor.get(), expected_cursor);
    }
}

#[test]
fn visual_indentation_shifts_each_line_by_the_requested_number_of_levels() {
    let (mut state, buffer, view) = editing("foo\n\tbar\n\n", 0);
    state.policy.tab_width = 8;
    state.policy.indent_width = 4;
    state.policy.expand_tabs = false;
    for (operator, expected, cursor) in [
        ("indent", "\t    foo\n\t\t    bar\n\n", 5),
        ("unindent", "foo\n\tbar\n\n", 0),
    ] {
        state.views[view].mode = crate::editor::EditorMode::VisualLine;
        state.views[view].selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(0),
            head: crate::editor::text::ByteOffset(state.buffers[buffer].text.len_bytes() - 1),
            kind: crate::editor::SelectionKind::Line,
        });
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", operator),
                ("motion", "visual"),
                ("linewise", "true"),
            ],
            3,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), cursor);
    }
}

#[test]
fn indentation_positions_the_cursor_on_the_first_affected_nonblank() {
    for (source, operator, expected, cursor) in [
        ("before\nfoo\n", "indent", "before\n    foo\n", 11),
        ("before\n        foo\n", "unindent", "before\n    foo\n", 11),
    ] {
        let (mut state, buffer, view) = editing(source, 7);
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", operator),
                ("motion", "line"),
                ("linewise", "true"),
            ],
            1,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), cursor);
        apply(&mut state, "edit.delete_char", &[], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "before\n    oo\n");
    }
}

#[test]
fn comment_templates_with_outer_whitespace_round_trip() {
    for template in [" <!-- %s -->", "<!-- %s --> ", " <!-- %s --> "] {
        let (mut state, buffer, _) = editing("  hello\n  world\n", 0);
        let arguments = [
            ("operator", "comment"),
            ("motion", "line"),
            ("linewise", "true"),
            ("comment", template),
        ];
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            "  <!-- hello -->\n  <!-- world -->\n"
        );
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), "  hello\n  world\n");
    }
}

#[test]
fn comment_insertion_uses_each_lines_unicode_whitespace_boundaries() {
    for (original, commented) in [
        ("\u{2003}one\n two\n", "\u{2003}// one\n // two\n"),
        (
            "\u{2003}\u{2003}one\n two\n",
            "\u{2003}// \u{2003}one\n // two\n",
        ),
        (" one\n\u{2003}two\n", " // one\n\u{2003}// two\n"),
    ] {
        let (mut state, buffer, _) = editing(original, 0);
        let arguments = [
            ("operator", "comment"),
            ("motion", "line"),
            ("linewise", "true"),
            ("comment", "//"),
        ];
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), commented);
        apply(&mut state, "edit.operator", &arguments, 2);
        assert_eq!(state.buffers[buffer].text.to_string(), original);
    }
}

#[test]
fn blank_comment_ranges_finish_the_operator_and_visual_modes() {
    for mode in [
        crate::editor::EditorMode::OperatorPending,
        crate::editor::EditorMode::VisualLine,
    ] {
        let (mut state, buffer, view) = editing("\n\n", 0);
        state.views[view].mode = mode;
        apply(
            &mut state,
            "edit.operator",
            &[
                ("operator", "comment"),
                ("motion", "line"),
                ("linewise", "true"),
                ("comment", "//"),
            ],
            2,
        );
        assert_eq!(state.views[view].mode, crate::editor::EditorMode::Normal);
        assert_eq!(state.buffers[buffer].text.to_string(), "\n\n");
    }
}
