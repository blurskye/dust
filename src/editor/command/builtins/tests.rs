use std::path::PathBuf;

use crate::editor::text::TextBuffer;

use crate::editor::{
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

    state.policy = crate::editor::EditingPolicy {
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
    assert!(
        state.buffers[state.views[view].buffer]
            .text
            .to_string()
            .starts_with('\t')
    );
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
    let (buffer, view) = state.open_text(TextBuffer::from_text(text), None);
    state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
    (state, buffer, view)
}

#[test]
fn linewise_put_separates_an_unterminated_last_line() {
    use crate::editor::{RegisterShape, RegisterValue};
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
fn block_put_extends_eof_and_repeats_rows_horizontally() {
    use crate::editor::{RegisterShape, RegisterValue};
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
    for (text, cursor, motion, character) in [
        ("(abc)", 4, "matching_pair", "("),
        ("abc", 2, "find_backward", "a"),
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
            1,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), "");
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
        assert_eq!(state.buffers[buffer].text.to_string(), "  xe\u{301} e\u{301}x");
    }
}

#[test]
fn multi_visual_matches_remain_literal_after_marking() {
    use crate::editor::{EditorMode, Selection, SelectionKind, text::ByteOffset};
    for command in ["multi.add_next", "multi.add_all", "multi.skip"] {
        let (mut state, _, view) = editing("foobar xfoo yfoo", 0);
        state.views[view].mode = EditorMode::VisualChar;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0), head: ByteOffset(2), kind: SelectionKind::Character,
        });
        apply(&mut state, "multi.add_next", &[], 1);
        apply(&mut state, command, &[], 1);
        assert!(state.views[view].multi_marks.iter().any(|mark| mark.start == ByteOffset(8)));
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
    use crate::editor::{EditorMode, text::{ByteOffset, ByteRange}};
    let (mut state, _, view) = editing("abcdef\nx\nabcdef\nabcdef\nx\nabcdef", 4);
    state.views[view].mode = EditorMode::Multi;
    state.views[view].multi_marks = vec![ByteRange::empty(ByteOffset(4)), ByteRange::empty(ByteOffset(18))];
    for (motion, expected) in [("down", vec![7, 23]), ("down", vec![13, 27]), ("up", vec![7, 23]), ("up", vec![4, 18])] {
        apply(&mut state, "multi.move", &[("motion", motion)], 1);
        assert_eq!(state.views[view].multi_marks.iter().map(|mark| mark.start.get()).collect::<Vec<_>>(), expected);
    }
    apply(&mut state, "multi.move", &[("motion", "down")], 1);
    apply(&mut state, "multi.move", &[("motion", "line_start")], 1);
    apply(&mut state, "multi.move", &[("motion", "down")], 1);
    assert_eq!(state.views[view].multi_marks.iter().map(|mark| mark.start.get()).collect::<Vec<_>>(), vec![9, 25]);
}

#[test]
fn adjacent_multi_character_deletions_deduplicate_collapsed_cursors() {
    use crate::editor::{EditorMode, text::{ByteOffset, ByteRange}};
    let (mut state, buffer, view) = editing("::x", 0);
    state.views[view].mode = EditorMode::Multi;
    state.views[view].multi_marks = vec![ByteRange::empty(ByteOffset(0)), ByteRange::empty(ByteOffset(1))];
    apply(&mut state, "multi.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "x");
    assert_eq!(state.views[view].multi_marks, vec![ByteRange::empty(ByteOffset(0))]);
    apply(&mut state, "multi.delete_char", &[], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "");
}

#[test]
fn multi_block_append_pads_short_rows_to_the_rectangle_edge() {
    use crate::editor::{EditorMode, Selection, SelectionKind, text::ByteOffset};
    for (source, expected) in [("abc\nx\nabc", "abc!\nx  !\nabc!"), ("x\nabc\nabc", "x  !\nabc!\nabc!"), ("abc\n\nabc", "abc!\n   !\nabc!")] {
        let (mut state, buffer, view) = editing(source, 0);
        state.views[view].mode = EditorMode::VisualBlock;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0), head: ByteOffset(source.len() - 1), kind: SelectionKind::Block,
        });
        apply(&mut state, "multi.insert", &[("placement", "end")], 1);
        assert_eq!(state.views[view].cursor, state.views[view].block_insert[0]);
        apply(&mut state, "edit.insert_text", &[("text", "!")], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn multi_block_change_deletes_the_supplied_visual_ranges() {
    use crate::editor::{EditorMode, Selection, SelectionKind, text::ByteOffset};
    let (mut state, buffer, view) = editing("abc\nx\nabc", 0);
    state.views[view].mode = EditorMode::VisualBlock;
    state.views[view].selection = Some(Selection {
        anchor: ByteOffset(0), head: ByteOffset(7), kind: SelectionKind::Block,
    });
    apply(&mut state, "multi.insert", &[("placement", "change")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "c\n\nc");
    apply(&mut state, "edit.insert_text", &[("text", "!")], 1);
    assert_eq!(state.buffers[buffer].text.to_string(), "!c\n!\n!c");
}

#[test]
fn adjacent_multi_changes_keep_one_insertion_at_each_resulting_position() {
    use crate::editor::text::{ByteOffset, ByteRange};
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
    assert!(state.buffers[buffer].text.to_string().ends_with('é'));
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
