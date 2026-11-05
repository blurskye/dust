use std::path::PathBuf;

use crate::editor::text::TextBuffer;

use crate::editor::{
    CommandInvocation, CommandRegistry, CommandValue, EditorState, InvocationSource,
    register_builtin_commands,
};

#[test]
fn movement_and_edit_commands_apply_through_registry_transactions() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state.open_text(TextBuffer::from_text("one two"), None).unwrap();
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
        assert_eq!(state.buffers[buffer].text.to_string(), source.replacen("\n\n", "\n", 1));
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
        assert_eq!(state.buffers[buffer].text.to_string(), expected, "{source:?}");
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
            &[("operator", operator), ("motion", "visual"), ("linewise", "true")],
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
            &[("operator", operator), ("motion", "line"), ("linewise", "true")],
            1,
        );
        assert_eq!(state.buffers[buffer].text.to_string(), expected);
        assert_eq!(state.views[view].cursor.get(), cursor);
        apply(&mut state, "edit.delete_char", &[], 1);
        assert_eq!(state.buffers[buffer].text.to_string(), "before\n    oo\n");
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
